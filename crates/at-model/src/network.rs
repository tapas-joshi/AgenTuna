//! The one network policy: which actions may reach the network, and where they may go.
//!
//! AgenTuna sends nothing anywhere unless the user asked for it or opted in: no telemetry, no
//! crash reporting, no account and no update without consent. Every egress site in the
//! workspace (git, the MCP HTTP client, the updater, the marketplace and remote backups) asks
//! [`NetworkPolicy`] first and names a [`NetworkAction`], so this policy — not the call site —
//! decides.
//!
//! Nothing here performs input or output. [`NetworkPolicy::authorize`] inspects settings and
//! strings only, and a caller must run it *before* it opens a socket or resolves a name; a
//! refusal therefore costs no connection. [`NetworkEventLog`] is a bounded, in-memory record
//! of calls that passed the policy: time, action, host and a byte count, never a payload or a
//! header.
//!
//! ```
//! use at_model::network::{NetworkAction, NetworkPolicy, NetworkTrigger};
//!
//! let policy = NetworkPolicy::default();
//! // A user-initiated call is permitted; nothing runs until the user asks for it.
//! assert!(policy.authorize(NetworkAction::GitFetch, "github.com", NetworkTrigger::User).is_ok());
//! // The same action from a background task is refused by the default policy.
//! assert!(
//!     policy
//!         .authorize(NetworkAction::GitFetch, "github.com", NetworkTrigger::Background)
//!         .is_err()
//! );
//! ```

use core::fmt;
use core::str::FromStr;
use std::collections::VecDeque;

use serde::Serialize;
use time::OffsetDateTime;
use url::Url;

use crate::error::{ErrorKind, ErrorReport, Remedy};
use crate::wire::wire_enum;

/// Maximum number of entries the local network event log keeps.
pub const NETWORK_EVENT_CAPACITY: usize = 20;

/// Longest host name accepted, in bytes (the DNS limit).
const MAX_HOST_BYTES: usize = 253;

wire_enum! {
    /// Every action in AgenTuna that is allowed to reach the network.
    ///
    /// The registry is closed by design: adding a new egress site means adding a variant here,
    /// a label and a trigger, and every gate in the workspace then has to name it. The wire
    /// name is the stable id used in settings, logs, the operations log and the CLI.
    #[non_exhaustive]
    pub enum NetworkAction {
        /// Fetching from a git remote.
        GitFetch => "git_fetch",
        /// Cloning a repository.
        GitClone => "git_clone",
        /// Searching or refreshing the skills marketplace.
        MarketplaceBrowse => "marketplace_browse",
        /// Installing a skill from the marketplace.
        MarketplaceInstall => "marketplace_install",
        /// Asking whether a newer AgenTuna release exists.
        UpdateCheck => "update_check",
        /// Downloading an update the user asked for.
        UpdateDownload => "update_download",
        /// Pushing a backup to a user-configured remote.
        RemoteBackupPush => "remote_backup_push",
        /// Restoring a backup from a user-configured remote.
        RemoteBackupPull => "remote_backup_pull",
        /// Connecting to a saved remote MCP server.
        McpRemoteConnect => "mcp_remote_connect",
        /// Signing in to a saved remote MCP server.
        McpOAuth => "mcp_oauth",
        /// The health scheduler checking an HTTP MCP server in the background.
        ScheduledHttpHealthCheck => "scheduled_http_health_check",
    }
}

impl NetworkAction {
    /// User-facing name of the action, shown in Settings and in the network gate.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GitFetch => "Fetch from a git remote",
            Self::GitClone => "Clone a repository",
            Self::MarketplaceBrowse => "Browse the skills marketplace",
            Self::MarketplaceInstall => "Install from the marketplace",
            Self::UpdateCheck => "Check for updates",
            Self::UpdateDownload => "Download an update",
            Self::RemoteBackupPush => "Push a backup to a remote",
            Self::RemoteBackupPull => "Restore from a remote backup",
            Self::McpRemoteConnect => "Connect to a remote MCP server",
            Self::McpOAuth => "Sign in to a remote MCP server",
            Self::ScheduledHttpHealthCheck => "Scheduled HTTP health check",
        }
    }

    /// The one situation that triggers this action.
    ///
    /// Every action has exactly one trigger, so Settings can promise what the app will do
    /// without describing code paths. Only [`Self::ScheduledHttpHealthCheck`] is started by
    /// AgenTuna itself, and only after the user opted in.
    #[must_use]
    pub const fn trigger(self) -> &'static str {
        match self {
            Self::GitFetch => "You fetch a remote in the source screen",
            Self::GitClone => "You clone a repository",
            Self::MarketplaceBrowse => "You open, search or refresh the marketplace",
            Self::MarketplaceInstall => "You install a marketplace skill",
            Self::UpdateCheck => "You choose Check for Updates, or a consented background check",
            Self::UpdateDownload => "You download an update after a check",
            Self::RemoteBackupPush => "You push a backup to a remote",
            Self::RemoteBackupPull => "You restore a backup from a remote",
            Self::McpRemoteConnect => "You open or test a saved remote server",
            Self::McpOAuth => "You sign in to a saved remote server",
            Self::ScheduledHttpHealthCheck => "The health scheduler checks an HTTP server",
        }
    }

    /// Whether only an explicit user gesture may run this action.
    ///
    /// A caller that is not reacting to the user must not invoke an explicit action. The one
    /// background action is [`Self::ScheduledHttpHealthCheck`], and it is still gated by
    /// [`BackgroundHealth::All`] and by the scheduler's own consent rules.
    #[must_use]
    pub const fn explicit(self) -> bool {
        !matches!(self, Self::ScheduledHttpHealthCheck)
    }

    /// Whether this action is started by AgenTuna rather than by the user.
    #[must_use]
    pub const fn is_background(self) -> bool {
        !self.explicit()
    }
}

wire_enum! {
    /// How much background health checking the user allowed (`background_health` in Settings).
    #[derive(Default)]
    #[non_exhaustive]
    pub enum BackgroundHealth {
        /// No scheduled checks at all.
        Off => "off",
        /// Only stdio servers that were tested once; HTTP checks need [`Self::All`].
        #[default]
        Local => "local",
        /// Scheduled checks for every saved server, including HTTP ones.
        All => "all",
    }
}

/// Who started a network action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NetworkTrigger {
    /// The user asked for this call right now: a click, a menu item or a CLI command.
    User,
    /// AgenTuna started the call itself: the health scheduler or a consented update check.
    Background,
}

impl NetworkTrigger {
    /// Whether the call was started by the user.
    #[must_use]
    pub const fn is_user(self) -> bool {
        matches!(self, Self::User)
    }
}

impl fmt::Display for NetworkTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::Background => "background",
        })
    }
}

/// A host name that is safe to compare, show and log.
///
/// Normalized on the way in: lower case, no trailing dot, no user info and no port. Parsing is
/// pure string work, so a value that fails here never reaches a name lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct NetworkHost(String);

impl NetworkHost {
    /// Normalizes `raw` as a host name.
    ///
    /// # Errors
    ///
    /// [`InvalidHost`] when the value is empty, longer than 253 bytes, contains a character
    /// that cannot appear in a host name, or has an empty label.
    pub fn parse(raw: &str) -> Result<Self, InvalidHost> {
        normalize(raw).map(Self).ok_or(InvalidHost)
    }

    /// Extracts the host a URL or a git remote points at.
    ///
    /// Accepts scheme URLs (`https://`, `ssh://`, `ws://`), scp-like git remotes
    /// (`git@github.com:owner/repo`), and bare host names. User info is dropped, never
    /// returned; a URL without a host (for example `file:///tmp/repo`) yields `None`.
    #[must_use]
    pub fn from_url(url: &str) -> Option<Self> {
        let raw = url.trim();
        if raw.is_empty() {
            return None;
        }
        if let Ok(parsed) = Url::parse(raw) {
            return parsed.host_str().and_then(|host| Self::parse(host).ok());
        }
        if raw.contains("://") {
            return None;
        }
        // scp-like git syntax: `[user@]host:path`.
        let (after_user, has_user) = match raw.rsplit_once('@') {
            Some((_user, rest)) => (rest, true),
            None => (raw, false),
        };
        if let Some((host, _path)) = after_user.split_once(':') {
            // A single-letter "host" is a drive letter, not a destination.
            if !has_user && !host.contains('.') {
                return None;
            }
            return Self::parse(host).ok();
        }
        Self::parse(raw).ok()
    }

    /// The normalized host.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `self` is `allowed` or a subdomain of it.
    #[must_use]
    pub fn is_or_subdomain_of(&self, allowed: &Self) -> bool {
        if self == allowed {
            return true;
        }
        self.0
            .strip_suffix(allowed.as_str())
            .is_some_and(|prefix| prefix.ends_with('.'))
    }
}

impl fmt::Display for NetworkHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NetworkHost {
    type Err = InvalidHost;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

fn normalize(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let host = if let Some(rest) = trimmed.strip_prefix('[') {
        let end = rest.find(']')?;
        let literal = &rest[..end];
        if literal.is_empty() {
            return None;
        }
        format!("[{literal}]")
    } else {
        let name = match trimmed.rsplit_once(':') {
            Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                name
            }
            _ => trimmed,
        };
        name.trim_end_matches('.').to_owned()
    };
    if host.is_empty() || host.len() > MAX_HOST_BYTES {
        return None;
    }
    if !host.bytes().all(valid_host_byte) {
        return None;
    }
    if !host.starts_with('[') && host.split('.').any(str::is_empty) {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn valid_host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b':' | b'[' | b']')
}

/// Returned by [`NetworkHost::parse`] when a value is not a host name.
///
/// The rejected value is not echoed: it may be a URL fragment that carries credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a valid network host")]
pub struct InvalidHost;

/// Why the policy refused a call.
///
/// The variant is what the network gate shows next to the action; the message is the sentence
/// the Settings privacy section displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum DenialReason {
    /// Offline mode is on.
    Offline,
    /// The action needs an opt-in that is currently off.
    ActionDisabled,
    /// Only a user gesture may run this action.
    BackgroundNotConsented,
    /// Only the health scheduler may run this action.
    BackgroundOnly,
    /// The host is not on the configured allow-list.
    HostNotAllowed,
    /// The destination host is missing or malformed.
    InvalidHost,
}

impl DenialReason {
    /// User-facing sentence for the privacy section and the network gate.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Offline => "Offline mode is on, so AgenTuna does not use the network.",
            Self::ActionDisabled => "This network action is turned off in Settings.",
            Self::BackgroundNotConsented => {
                "AgenTuna only uses this action while you are working, not in the background."
            }
            Self::BackgroundOnly => {
                "The health scheduler runs this action; a manual check uses the connect action."
            }
            Self::HostNotAllowed => "The destination host is not on the network allow-list.",
            Self::InvalidHost => "The destination host is missing or not a valid host name.",
        }
    }

    /// Stable id used in JSON and in the UI.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::ActionDisabled => "action_disabled",
            Self::BackgroundNotConsented => "background_not_consented",
            Self::BackgroundOnly => "background_only",
            Self::HostNotAllowed => "host_not_allowed",
            Self::InvalidHost => "invalid_host",
        }
    }
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// A refused network call, produced before any connection is attempted.
///
/// Gateways return this instead of running the egress call, so the host app can show the
/// reason and offer Settings; retrying the same call cannot help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkDenied {
    /// Action that was refused.
    pub action: NetworkAction,
    /// Normalized destination host, when the policy got far enough to know it.
    pub host: Option<NetworkHost>,
    /// Why it was refused.
    pub reason: DenialReason,
}

impl NetworkDenied {
    /// Builds a refusal.
    #[must_use]
    pub const fn new(
        action: NetworkAction,
        host: Option<NetworkHost>,
        reason: DenialReason,
    ) -> Self {
        Self {
            action,
            host,
            reason,
        }
    }

    /// A refusal because the caller had no valid destination host.
    #[must_use]
    pub const fn invalid_host(action: NetworkAction) -> Self {
        Self::new(action, None, DenialReason::InvalidHost)
    }

    /// The report the CLI prints and the app renders as a toast with an Open Settings action.
    #[must_use]
    pub fn report(&self) -> ErrorReport {
        ErrorReport::new(ErrorKind::NetworkDenied, self.to_string())
            .with_remedies([Remedy::OpenSettings, Remedy::CopyDetails])
            .with_retryable(false)
    }
}

impl fmt::Display for NetworkDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Some(host) => write!(
                f,
                "{} to {host} was blocked: {}",
                self.action.label(),
                self.reason
            ),
            None => write!(f, "{} was blocked: {}", self.action.label(), self.reason),
        }
    }
}

impl std::error::Error for NetworkDenied {}

/// Presentation state of one action in the network gate and the Settings privacy section.
///
/// The three states are exhaustive by construction of [`NetworkPolicy::state`]: an action is
/// either refused with a reason, permitted only after the user confirms this exact call, or
/// already consented for background use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NetworkActionState {
    /// Permitted, but only after the user confirms this call, naming the destination host.
    ConfirmationRequired,
    /// Permitted without a further prompt because the user opted in to background use.
    Available,
    /// Not permitted; the policy never opens a connection for it.
    Blocked(DenialReason),
}

impl NetworkActionState {
    /// Stable id used in JSON and in the UI (`available`, `confirmation_required`, `blocked`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ConfirmationRequired => "confirmation_required",
            Self::Available => "available",
            Self::Blocked(_) => "blocked",
        }
    }

    /// The refusal reason, when blocked.
    #[must_use]
    pub const fn reason(self) -> Option<DenialReason> {
        match self {
            Self::Blocked(reason) => Some(reason),
            Self::ConfirmationRequired | Self::Available => None,
        }
    }
}

/// One registry row: the action, its label, its trigger and its current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkActionStatus {
    /// Action this row describes.
    pub action: NetworkAction,
    /// User-facing label.
    pub label: &'static str,
    /// The one situation that triggers the action.
    pub trigger: &'static str,
    /// Whether only an explicit user gesture may run it.
    pub explicit: bool,
    /// Current state under the policy that produced the row.
    pub state: NetworkActionState,
}

/// The network policy: settings plus an optional destination allow-list.
///
/// Built from the user's settings (`network.marketplace_enabled`, `background_health`) and, when
/// configured, a host allow-list. The default policy — an unconfigured install — performs no
/// background network access and keeps the marketplace off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkPolicy {
    offline: bool,
    marketplace_enabled: bool,
    background_health: BackgroundHealth,
    updates_opt_in: bool,
    allowed_hosts: Vec<NetworkHost>,
}

impl NetworkPolicy {
    /// The default policy; identical to [`NetworkPolicy::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns offline mode on or off. Offline mode refuses every action, including user ones.
    #[must_use]
    pub fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// Applies the `network.marketplace_enabled` setting.
    #[must_use]
    pub fn with_marketplace_enabled(mut self, enabled: bool) -> Self {
        self.marketplace_enabled = enabled;
        self
    }

    /// Applies the `background_health` setting.
    #[must_use]
    pub fn with_background_health(mut self, scope: BackgroundHealth) -> Self {
        self.background_health = scope;
        self
    }

    /// Records the user's consent to background update checks and downloads.
    ///
    /// This is the policy form of the `update_check` setting (TF-032): `automatic` sets it,
    /// `manual` clears it. It never enables anything else — a background git fetch, marketplace
    /// browse or MCP connection stays refused even with consent.
    #[must_use]
    pub fn with_updates_opt_in(mut self, opted_in: bool) -> Self {
        self.updates_opt_in = opted_in;
        self
    }

    /// Replaces the host allow-list. Invalid entries are dropped: they could never match, and
    /// keeping them would make the Settings list lie.
    #[must_use]
    pub fn with_allowed_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.allowed_hosts = hosts
            .into_iter()
            .filter_map(|host| NetworkHost::parse(host.as_ref()).ok())
            .fold(Vec::new(), |mut unique, host| {
                if !unique.contains(&host) {
                    unique.push(host);
                }
                unique
            });
        self
    }

    /// Whether offline mode is on.
    #[must_use]
    pub const fn offline(&self) -> bool {
        self.offline
    }

    /// Whether the marketplace is enabled.
    #[must_use]
    pub const fn marketplace_enabled(&self) -> bool {
        self.marketplace_enabled
    }

    /// The `background_health` setting.
    #[must_use]
    pub const fn background_health(&self) -> BackgroundHealth {
        self.background_health
    }

    /// Whether the user consented to background updates (`update_check=automatic`).
    #[must_use]
    pub const fn updates_opt_in(&self) -> bool {
        self.updates_opt_in
    }

    /// The configured host allow-list; empty means "any host for a permitted action".
    #[must_use]
    pub fn allowed_hosts(&self) -> &[NetworkHost] {
        &self.allowed_hosts
    }

    /// Settings-level decision for `action` and `host`.
    ///
    /// Use this for a pre-flight check or to describe an action. It ignores who started the
    /// call; [`Self::authorize`] adds that rule and is what egress sites must call.
    #[must_use]
    pub fn permits(&self, action: NetworkAction, host: &str) -> bool {
        self.check(action, host).is_ok()
    }

    /// Settings-level decision that returns the normalized host on success.
    ///
    /// # Errors
    ///
    /// [`NetworkDenied`] with [`DenialReason::Offline`], [`DenialReason::ActionDisabled`],
    /// [`DenialReason::HostNotAllowed`] or [`DenialReason::InvalidHost`].
    pub fn check(&self, action: NetworkAction, host: &str) -> Result<NetworkHost, NetworkDenied> {
        self.decide(action, host, None)
    }

    /// The full gate: settings, host and trigger. Egress sites call this first and only then
    /// open a connection.
    ///
    /// # Errors
    ///
    /// [`NetworkDenied`] for every refusal; the reason tells the gate what to show.
    pub fn authorize(
        &self,
        action: NetworkAction,
        host: &str,
        trigger: NetworkTrigger,
    ) -> Result<NetworkHost, NetworkDenied> {
        self.decide(action, host, Some(trigger))
    }

    /// The presentation state of `action`, including whether a confirmation is still needed.
    #[must_use]
    pub fn state(&self, action: NetworkAction) -> NetworkActionState {
        if self.offline {
            return NetworkActionState::Blocked(DenialReason::Offline);
        }
        if let Some(reason) = self.action_denial(action) {
            return NetworkActionState::Blocked(reason);
        }
        if action.explicit() && !self.background_consent(action) {
            NetworkActionState::ConfirmationRequired
        } else {
            NetworkActionState::Available
        }
    }

    /// One sentence the Settings privacy section shows above the action list.
    ///
    /// The text lives here so the GUI, the CLI and a future export describe the same promise; an
    /// offline policy says so instead of describing requests that cannot happen.
    #[must_use]
    pub fn statement(&self) -> &'static str {
        if self.offline {
            "Offline mode is on: AgenTuna does not use the network, and every action below is \
             blocked until you turn it off."
        } else {
            "AgenTuna has no telemetry, crash reporting or account, and makes no network request \
             unless one of the actions below is triggered."
        }
    }

    /// One row per registered action, in registry order, for the Settings privacy section.
    #[must_use]
    pub fn statuses(&self) -> Vec<NetworkActionStatus> {
        NetworkAction::ALL
            .iter()
            .map(|&action| NetworkActionStatus {
                action,
                label: action.label(),
                trigger: action.trigger(),
                explicit: action.explicit(),
                state: self.state(action),
            })
            .collect()
    }

    fn decide(
        &self,
        action: NetworkAction,
        host: &str,
        trigger: Option<NetworkTrigger>,
    ) -> Result<NetworkHost, NetworkDenied> {
        let parsed = NetworkHost::parse(host);
        if self.offline {
            return Err(NetworkDenied::new(
                action,
                parsed.ok(),
                DenialReason::Offline,
            ));
        }
        if let Some(reason) = self.action_denial(action) {
            return Err(NetworkDenied::new(action, parsed.ok(), reason));
        }
        if let Some(trigger) = trigger
            && let Some(reason) = self.trigger_denial(action, trigger)
        {
            return Err(NetworkDenied::new(action, parsed.ok(), reason));
        }
        let host = parsed.map_err(|_| NetworkDenied::invalid_host(action))?;
        if !self.host_allowed(&host) {
            return Err(NetworkDenied::new(
                action,
                Some(host),
                DenialReason::HostNotAllowed,
            ));
        }
        Ok(host)
    }

    fn action_denial(&self, action: NetworkAction) -> Option<DenialReason> {
        match action {
            NetworkAction::MarketplaceBrowse | NetworkAction::MarketplaceInstall => {
                if self.marketplace_enabled {
                    None
                } else {
                    Some(DenialReason::ActionDisabled)
                }
            }
            NetworkAction::ScheduledHttpHealthCheck => {
                if self.background_health == BackgroundHealth::All {
                    None
                } else {
                    Some(DenialReason::ActionDisabled)
                }
            }
            NetworkAction::GitFetch
            | NetworkAction::GitClone
            | NetworkAction::UpdateCheck
            | NetworkAction::UpdateDownload
            | NetworkAction::RemoteBackupPush
            | NetworkAction::RemoteBackupPull
            | NetworkAction::McpRemoteConnect
            | NetworkAction::McpOAuth => None,
        }
    }

    fn trigger_denial(
        &self,
        action: NetworkAction,
        trigger: NetworkTrigger,
    ) -> Option<DenialReason> {
        match (trigger, action) {
            (NetworkTrigger::User, NetworkAction::ScheduledHttpHealthCheck) => {
                Some(DenialReason::BackgroundOnly)
            }
            (NetworkTrigger::Background, NetworkAction::ScheduledHttpHealthCheck) => None,
            (
                NetworkTrigger::Background,
                NetworkAction::UpdateCheck | NetworkAction::UpdateDownload,
            ) => {
                if self.updates_opt_in {
                    None
                } else {
                    Some(DenialReason::BackgroundNotConsented)
                }
            }
            (NetworkTrigger::Background, _) => Some(DenialReason::BackgroundNotConsented),
            (NetworkTrigger::User, _) => None,
        }
    }

    fn background_consent(&self, action: NetworkAction) -> bool {
        matches!(
            action,
            NetworkAction::UpdateCheck | NetworkAction::UpdateDownload
        ) && self.updates_opt_in
    }

    fn host_allowed(&self, host: &NetworkHost) -> bool {
        self.allowed_hosts.is_empty()
            || self
                .allowed_hosts
                .iter()
                .any(|allowed| host.is_or_subdomain_of(allowed))
    }
}

/// One entry of the local network event log.
///
/// Exactly four fields: when, which action, which host and how many bytes crossed the socket.
/// No URL path, query, payload or header is stored, so the log cannot leak a secret or a
/// server response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkEvent {
    /// When the call started.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Which action was permitted.
    pub action: NetworkAction,
    /// Destination host, when the call had one.
    pub host: Option<NetworkHost>,
    /// Bytes sent and received, as reported by the caller; `0` when unknown.
    pub bytes: u64,
}

impl NetworkEvent {
    /// Builds an entry.
    #[must_use]
    pub const fn new(
        at: OffsetDateTime,
        action: NetworkAction,
        host: Option<NetworkHost>,
        bytes: u64,
    ) -> Self {
        Self {
            at,
            action,
            host,
            bytes,
        }
    }
}

/// The local, in-memory ring buffer of network events.
///
/// Holds at most [`NETWORK_EVENT_CAPACITY`] entries; the oldest is evicted when a new one
/// arrives. It never leaves the machine and is never written to disk or included in a
/// diagnostic export.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkEventLog {
    entries: VecDeque<NetworkEvent>,
}

impl NetworkEventLog {
    /// Maximum number of entries kept.
    pub const CAPACITY: usize = NETWORK_EVENT_CAPACITY;

    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one event, evicting the oldest entry when the log is full.
    pub fn record(&mut self, event: NetworkEvent) {
        while self.entries.len() >= Self::CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(event);
    }

    /// Number of stored events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no event is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Oldest first.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &NetworkEvent> + ExactSizeIterator {
        self.entries.iter()
    }

    /// A copy of the stored events, oldest first.
    #[must_use]
    pub fn to_vec(&self) -> Vec<NetworkEvent> {
        self.entries.iter().cloned().collect()
    }

    /// Forgets every stored event. The Settings privacy section calls this from its Clear button.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ACTIONS: [NetworkAction; 11] = [
        NetworkAction::GitFetch,
        NetworkAction::GitClone,
        NetworkAction::MarketplaceBrowse,
        NetworkAction::MarketplaceInstall,
        NetworkAction::UpdateCheck,
        NetworkAction::UpdateDownload,
        NetworkAction::RemoteBackupPush,
        NetworkAction::RemoteBackupPull,
        NetworkAction::McpRemoteConnect,
        NetworkAction::McpOAuth,
        NetworkAction::ScheduledHttpHealthCheck,
    ];

    fn at(offset_seconds: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(offset_seconds)
    }

    #[test]
    fn registry_has_exactly_the_eleven_actions() {
        assert_eq!(NetworkAction::ALL, ALL_ACTIONS.as_slice());
        assert_eq!(NetworkAction::ALL.len(), 11);
        assert!(NetworkAction::ALL.contains(&NetworkAction::ScheduledHttpHealthCheck));
    }

    #[test]
    fn every_action_has_one_label_and_one_trigger() {
        for action in ALL_ACTIONS {
            assert!(!action.label().is_empty(), "{action} has no label");
            assert!(!action.trigger().is_empty(), "{action} has no trigger");
        }
        for (index, action) in ALL_ACTIONS.iter().enumerate() {
            for other in &ALL_ACTIONS[index + 1..] {
                assert_ne!(
                    action.label(),
                    other.label(),
                    "{action} and {other} share a label"
                );
                assert_ne!(
                    action.trigger(),
                    other.trigger(),
                    "{action} and {other} share a trigger"
                );
                assert_ne!(action.wire_name(), other.wire_name());
            }
        }
    }

    #[test]
    fn only_the_scheduled_check_is_not_explicit() {
        for action in ALL_ACTIONS {
            assert_eq!(
                action.explicit(),
                action != NetworkAction::ScheduledHttpHealthCheck,
                "{action}"
            );
            assert_eq!(action.is_background(), !action.explicit());
        }
    }

    #[test]
    fn scheduled_http_check_needs_background_health_all() {
        for scope in [BackgroundHealth::Off, BackgroundHealth::Local] {
            let policy = NetworkPolicy::default().with_background_health(scope);
            assert!(!policy.permits(NetworkAction::ScheduledHttpHealthCheck, "health.test"));
            assert_eq!(
                policy.state(NetworkAction::ScheduledHttpHealthCheck),
                NetworkActionState::Blocked(DenialReason::ActionDisabled)
            );
        }
        let policy = NetworkPolicy::default().with_background_health(BackgroundHealth::All);
        assert!(policy.permits(NetworkAction::ScheduledHttpHealthCheck, "health.test"));
        assert_eq!(
            policy.state(NetworkAction::ScheduledHttpHealthCheck),
            NetworkActionState::Available
        );
        assert!(
            policy
                .authorize(
                    NetworkAction::ScheduledHttpHealthCheck,
                    "health.test",
                    NetworkTrigger::Background
                )
                .is_ok()
        );
    }

    #[test]
    fn a_manual_trigger_cannot_run_the_scheduled_check() {
        let policy = NetworkPolicy::default().with_background_health(BackgroundHealth::All);
        let denied = policy
            .authorize(
                NetworkAction::ScheduledHttpHealthCheck,
                "health.test",
                NetworkTrigger::User,
            )
            .unwrap_err();
        assert_eq!(denied.reason, DenialReason::BackgroundOnly);
        assert!(policy.permits(NetworkAction::ScheduledHttpHealthCheck, "health.test"));
    }

    #[test]
    fn default_policy_allows_user_actions_and_no_background_action() {
        let policy = NetworkPolicy::default();
        for action in ALL_ACTIONS {
            let user = policy.authorize(action, "example.test", NetworkTrigger::User);
            let background = policy.authorize(action, "example.test", NetworkTrigger::Background);
            match action {
                NetworkAction::MarketplaceBrowse | NetworkAction::MarketplaceInstall => {
                    assert_eq!(user.unwrap_err().reason, DenialReason::ActionDisabled);
                }
                NetworkAction::ScheduledHttpHealthCheck => {
                    // The default policy is background_health=local, so the opt-in is what
                    // blocks it first; the trigger rule is reached only once it is on.
                    assert_eq!(user.unwrap_err().reason, DenialReason::ActionDisabled);
                }
                _ => assert!(user.is_ok(), "{action}"),
            }
            assert!(
                background.is_err(),
                "{action} must be refused in the background by the default policy"
            );
        }
    }

    #[test]
    fn marketplace_needs_the_opt_in() {
        let off = NetworkPolicy::default();
        assert!(!off.permits(NetworkAction::MarketplaceBrowse, "skills.test"));
        assert!(!off.permits(NetworkAction::MarketplaceInstall, "skills.test"));
        assert_eq!(
            off.state(NetworkAction::MarketplaceBrowse),
            NetworkActionState::Blocked(DenialReason::ActionDisabled)
        );

        let on = NetworkPolicy::default().with_marketplace_enabled(true);
        assert!(on.permits(NetworkAction::MarketplaceBrowse, "skills.test"));
        assert_eq!(
            on.state(NetworkAction::MarketplaceBrowse),
            NetworkActionState::ConfirmationRequired
        );
    }

    #[test]
    fn offline_denies_everything() {
        let policy = NetworkPolicy::default()
            .with_marketplace_enabled(true)
            .with_background_health(BackgroundHealth::All)
            .with_updates_opt_in(true)
            .with_offline(true);
        for action in ALL_ACTIONS {
            for trigger in [NetworkTrigger::User, NetworkTrigger::Background] {
                let denied = policy
                    .authorize(action, "example.test", trigger)
                    .unwrap_err();
                assert_eq!(denied.reason, DenialReason::Offline, "{action}/{trigger}");
                assert_eq!(
                    policy.state(action),
                    NetworkActionState::Blocked(DenialReason::Offline)
                );
            }
        }
    }

    #[test]
    fn background_updates_need_consent() {
        let policy = NetworkPolicy::default();
        assert!(
            policy
                .authorize(
                    NetworkAction::UpdateCheck,
                    "updates.test",
                    NetworkTrigger::User
                )
                .is_ok()
        );
        assert_eq!(
            policy
                .authorize(
                    NetworkAction::UpdateCheck,
                    "updates.test",
                    NetworkTrigger::Background
                )
                .unwrap_err()
                .reason,
            DenialReason::BackgroundNotConsented
        );

        let consented = policy.clone().with_updates_opt_in(true);
        assert!(
            consented
                .authorize(
                    NetworkAction::UpdateCheck,
                    "updates.test",
                    NetworkTrigger::Background
                )
                .is_ok()
        );
        assert_eq!(
            consented.state(NetworkAction::UpdateCheck),
            NetworkActionState::Available
        );
        assert_eq!(
            policy.state(NetworkAction::UpdateDownload),
            NetworkActionState::ConfirmationRequired
        );
    }

    #[test]
    fn other_actions_never_run_in_the_background_even_with_consent() {
        let policy = NetworkPolicy::default()
            .with_marketplace_enabled(true)
            .with_updates_opt_in(true);
        for action in [
            NetworkAction::GitFetch,
            NetworkAction::GitClone,
            NetworkAction::MarketplaceBrowse,
            NetworkAction::MarketplaceInstall,
            NetworkAction::RemoteBackupPush,
            NetworkAction::RemoteBackupPull,
            NetworkAction::McpRemoteConnect,
            NetworkAction::McpOAuth,
        ] {
            assert_eq!(
                policy
                    .authorize(action, "example.test", NetworkTrigger::Background)
                    .unwrap_err()
                    .reason,
                DenialReason::BackgroundNotConsented,
                "{action}"
            );
        }
    }

    #[test]
    fn allow_list_matches_exact_hosts_and_subdomains() {
        let policy = NetworkPolicy::default().with_allowed_hosts([
            "github.com",
            "API.Example.TEST.",
            "github.com",
        ]);
        assert_eq!(policy.allowed_hosts().len(), 2);
        assert!(policy.permits(NetworkAction::GitFetch, "github.com"));
        assert!(policy.permits(NetworkAction::GitFetch, "GITHUB.com"));
        assert!(policy.permits(NetworkAction::GitFetch, "gist.github.com"));
        assert!(policy.permits(NetworkAction::GitFetch, "api.example.test"));
        assert!(!policy.permits(NetworkAction::GitFetch, "evil.test"));
        assert!(!policy.permits(NetworkAction::GitFetch, "notgithub.com"));
        assert!(!policy.permits(NetworkAction::GitFetch, "github.com.evil.test"));
        assert_eq!(
            policy
                .check(NetworkAction::GitFetch, "evil.test")
                .unwrap_err()
                .reason,
            DenialReason::HostNotAllowed
        );
    }

    #[test]
    fn invalid_hosts_are_denied_and_never_echoed() {
        let policy = NetworkPolicy::default();
        for raw in [
            "",
            "   ",
            "https://example.test/repo",
            "user:secret@host",
            "/tmp/repo",
        ] {
            let denied = policy
                .authorize(NetworkAction::GitClone, raw, NetworkTrigger::User)
                .unwrap_err();
            assert_eq!(denied.reason, DenialReason::InvalidHost, "{raw:?}");
            assert!(denied.host.is_none());
            assert!(!denied.to_string().contains("secret"));
        }
    }

    #[test]
    fn hosts_normalize_case_trailing_dot_and_port() {
        assert_eq!(
            NetworkHost::parse("API.Example.TEST.").unwrap().as_str(),
            "api.example.test"
        );
        assert_eq!(
            NetworkHost::parse("example.test:8443").unwrap().as_str(),
            "example.test"
        );
        assert_eq!(NetworkHost::parse("[::1]:8080").unwrap().as_str(), "[::1]");
        assert!(NetworkHost::parse("exa mple.test").is_err());
        assert!(NetworkHost::parse("a..b").is_err());
        assert!(NetworkHost::parse(&"a".repeat(MAX_HOST_BYTES + 1)).is_err());
        // Credentials are never part of a host, and never accepted as one.
        assert!(NetworkHost::parse("user:pa55@example.test").is_err());
    }

    #[test]
    fn hosts_are_extracted_from_urls_and_git_remotes() {
        let cases = [
            ("https://github.com/acme/repo.git", "github.com"),
            ("https://user:token@github.com/acme/repo", "github.com"),
            ("ssh://git@github.com:22/acme/repo.git", "github.com"),
            ("git@github.com:acme/repo.git", "github.com"),
            ("example.test", "example.test"),
            ("wss://api.example.test:443/mcp", "api.example.test"),
        ];
        for (raw, expected) in cases {
            let host = NetworkHost::from_url(raw).unwrap_or_else(|| panic!("{raw}"));
            assert_eq!(host.as_str(), expected, "{raw}");
        }
        assert!(NetworkHost::from_url("file:///tmp/repo").is_none());
        assert!(NetworkHost::from_url("/tmp/repo").is_none());
        assert!(NetworkHost::from_url("C:\\repo").is_none());
    }

    #[test]
    fn the_privacy_statement_reflects_offline_mode() {
        let online = NetworkPolicy::default();
        assert!(online.statement().contains("no telemetry"));
        assert!(online.statement().contains("no network request"));
        let offline = online.clone().with_offline(true);
        assert!(offline.statement().contains("Offline mode is on"));
        assert_ne!(online.statement(), offline.statement());
    }

    #[test]
    fn statuses_list_every_action_with_its_state() {
        let policy = NetworkPolicy::default();
        let statuses = policy.statuses();
        assert_eq!(statuses.len(), 11);
        for (status, action) in statuses.iter().zip(ALL_ACTIONS) {
            assert_eq!(status.action, action);
            assert_eq!(status.label, action.label());
            assert_eq!(status.trigger, action.trigger());
            assert_eq!(status.explicit, action.explicit());
            assert_eq!(status.state, policy.state(action));
        }
    }

    #[test]
    fn event_log_keeps_the_newest_twenty() {
        assert_eq!(NETWORK_EVENT_CAPACITY, 20);
        let mut log = NetworkEventLog::new();
        assert!(log.is_empty());
        for index in 0..25 {
            log.record(NetworkEvent::new(
                at(index),
                NetworkAction::GitFetch,
                Some(NetworkHost::parse("github.com").unwrap()),
                index.unsigned_abs(),
            ));
        }
        assert_eq!(log.len(), 20);
        let entries = log.to_vec();
        assert_eq!(entries.first().unwrap().at, at(5));
        assert_eq!(entries.last().unwrap().at, at(24));
        assert_eq!(entries.first().unwrap().bytes, 5);
    }

    #[test]
    fn event_log_clear_forgets_everything() {
        let mut log = NetworkEventLog::new();
        log.record(NetworkEvent::new(
            at(0),
            NetworkAction::McpRemoteConnect,
            None,
            0,
        ));
        assert_eq!(log.entries().len(), 1);
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.to_vec(), Vec::new());
    }

    #[test]
    fn event_json_carries_no_payload_or_headers() {
        let event = NetworkEvent::new(
            at(0),
            NetworkAction::McpRemoteConnect,
            Some(NetworkHost::parse("mcp.example.test").unwrap()),
            512,
        );
        let value = serde_json::to_value(&event).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["action", "at", "bytes", "host"]);
        assert_eq!(value["action"], "mcp_remote_connect");
        assert_eq!(value["host"], "mcp.example.test");
    }

    #[test]
    fn denial_report_names_the_action_and_offers_settings() {
        let denied = NetworkDenied::new(
            NetworkAction::GitFetch,
            Some(NetworkHost::parse("github.com").unwrap()),
            DenialReason::Offline,
        );
        let report = denied.report();
        assert_eq!(report.kind, ErrorKind::NetworkDenied);
        assert!(!report.retryable);
        assert!(report.remedy.contains(&Remedy::OpenSettings));
        assert!(report.message.contains("github.com"));
        assert_eq!(report.kind.code(), "E_NETWORK_DENIED");
    }

    #[test]
    fn wire_names_round_trip() {
        for action in ALL_ACTIONS {
            assert_eq!(action.wire_name().parse::<NetworkAction>().unwrap(), action);
        }
        for scope in [
            BackgroundHealth::Off,
            BackgroundHealth::Local,
            BackgroundHealth::All,
        ] {
            assert_eq!(
                scope.wire_name().parse::<BackgroundHealth>().unwrap(),
                scope
            );
        }
        assert_eq!(BackgroundHealth::default(), BackgroundHealth::Local);
    }
}
