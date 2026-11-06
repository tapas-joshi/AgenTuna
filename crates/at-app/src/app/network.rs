//! Network policy state for the desktop app: the one policy, the event ring buffer and the
//! updater gate.
//!
//! The app owns a single [`NetworkState`]: it holds the [`NetworkPolicy`] the shell currently
//! applies and the bounded [`NetworkEventLog`] the privacy section shows. Commands in this
//! module only read that state or flip the user-facing offline switch; they never perform a
//! network call themselves.
//!
//! The updater has exactly one way in, [`authorize_update`]. An update check or download is
//! permitted when the user triggered it, or in the background only after the user opted in —
//! which is why the default policy keeps background updates off.
//!
//! Tauri commands here return plain, secret-free views: an action's label, trigger, state and
//! reason, and the last twenty events with their time, action, host and byte count. No payload,
//! URL path or header ever enters this module.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use at_model::network::{
    BackgroundHealth, NETWORK_EVENT_CAPACITY, NetworkAction, NetworkActionState, NetworkDenied,
    NetworkEvent, NetworkEventLog, NetworkHost, NetworkPolicy, NetworkTrigger,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use ts_rs::TS;

/// Actions the updater may use. Anything else is a programming error.
pub const UPDATE_ACTIONS: [NetworkAction; 2] =
    [NetworkAction::UpdateCheck, NetworkAction::UpdateDownload];

/// Process-wide network policy and event log, managed by the Tauri app.
///
/// Every command that needs a policy decision or renders the privacy section reads this one
/// value; the policy is read often, the log is written rarely, so the policy sits behind a
/// read-write lock and the log behind a mutex. Poisoning is recovered rather than propagated: a
/// stale lock must not disable offline mode.
#[derive(Clone)]
pub struct NetworkState {
    policy: Arc<RwLock<NetworkPolicy>>,
    events: Arc<Mutex<NetworkEventLog>>,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self::new(NetworkPolicy::default())
    }
}

impl NetworkState {
    /// Creates state for `policy` with an empty event log.
    #[must_use]
    pub fn new(policy: NetworkPolicy) -> Self {
        Self {
            policy: Arc::new(RwLock::new(policy)),
            events: Arc::new(Mutex::new(NetworkEventLog::new())),
        }
    }

    /// A copy of the current policy.
    #[must_use]
    pub fn policy(&self) -> NetworkPolicy {
        read_policy(&self.policy).clone()
    }

    /// Replaces the policy, for example after a settings write.
    pub fn set_policy(&self, policy: NetworkPolicy) {
        *write_policy(&self.policy) = policy;
    }

    /// Turns offline mode on or off.
    pub fn set_offline(&self, offline: bool) {
        let mut policy = write_policy(&self.policy);
        let next = policy.clone().with_offline(offline);
        *policy = next;
    }

    /// Applies the `network.marketplace_enabled` setting.
    pub fn set_marketplace_enabled(&self, enabled: bool) {
        let mut policy = write_policy(&self.policy);
        let next = policy.clone().with_marketplace_enabled(enabled);
        *policy = next;
    }

    /// Applies the `background_health` setting.
    pub fn set_background_health(&self, scope: BackgroundHealth) {
        let mut policy = write_policy(&self.policy);
        let next = policy.clone().with_background_health(scope);
        *policy = next;
    }

    /// Records the user's consent to background updates (`update_check=automatic`).
    pub fn set_updates_opt_in(&self, opted_in: bool) {
        let mut policy = write_policy(&self.policy);
        let next = policy.clone().with_updates_opt_in(opted_in);
        *policy = next;
    }

    /// Asks the current policy whether `action` may reach `host`.
    ///
    /// # Errors
    ///
    /// [`NetworkDenied`] when the policy refuses.
    pub fn authorize(
        &self,
        action: NetworkAction,
        host: &str,
        trigger: NetworkTrigger,
    ) -> Result<NetworkHost, NetworkDenied> {
        let policy = read_policy(&self.policy);
        policy.authorize(action, host, trigger)
    }

    /// Appends one event, evicting the oldest when the log is full.
    pub fn record(&self, event: NetworkEvent) {
        lock_events(&self.events).record(event);
    }

    /// A copy of the stored events, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<NetworkEvent> {
        lock_events(&self.events).to_vec()
    }

    /// The number of stored events.
    #[must_use]
    pub fn event_count(&self) -> usize {
        lock_events(&self.events).len()
    }

    /// Forgets every stored event; the Settings Clear button calls this.
    pub fn clear_events(&self) {
        lock_events(&self.events).clear();
    }
}

fn read_policy(policy: &RwLock<NetworkPolicy>) -> RwLockReadGuard<'_, NetworkPolicy> {
    policy.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_policy(policy: &RwLock<NetworkPolicy>) -> RwLockWriteGuard<'_, NetworkPolicy> {
    policy.write().unwrap_or_else(PoisonError::into_inner)
}

fn lock_events(events: &Mutex<NetworkEventLog>) -> MutexGuard<'_, NetworkEventLog> {
    events.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The updater's only way in.
///
/// A user-triggered check or download passes when the policy permits the action and the host. A
/// background one additionally needs the user's opt-in; the default policy refuses it, so an
/// unconfigured install makes no update request at launch or while idle.
///
/// # Errors
///
/// [`UpdateGateError::NotAnUpdateAction`] for any other action and [`UpdateGateError::Denied`]
/// when the policy refuses.
pub fn authorize_update(
    policy: &NetworkPolicy,
    action: NetworkAction,
    host: &str,
    trigger: NetworkTrigger,
) -> Result<NetworkHost, UpdateGateError> {
    if !UPDATE_ACTIONS.contains(&action) {
        return Err(UpdateGateError::NotAnUpdateAction(action));
    }
    policy
        .authorize(action, host, trigger)
        .map_err(UpdateGateError::Denied)
}

/// Failure of the updater gate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateGateError {
    /// The caller named an action the updater does not own.
    #[error("`{0}` is not an update action")]
    NotAnUpdateAction(NetworkAction),
    /// The policy refused the update call; no request was made.
    #[error(transparent)]
    Denied(NetworkDenied),
}

impl UpdateGateError {
    /// The policy refusal, when this failure is one.
    #[must_use]
    pub fn denied(&self) -> Option<&NetworkDenied> {
        match self {
            Self::Denied(denied) => Some(denied),
            Self::NotAnUpdateAction(_) => None,
        }
    }
}

/// Presentation state of one action, as the network gate and the privacy section render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum NetworkActionStateView {
    /// Permitted without a further prompt.
    Available,
    /// Permitted only after the user confirms this exact call.
    ConfirmationRequired,
    /// Refused; `reason` on the parent row says why.
    Blocked,
}

impl From<NetworkActionState> for NetworkActionStateView {
    /// Maps the policy state onto its wire id.
    ///
    /// A state that at-model gains later is rendered as blocked rather than allowed, so a new
    /// policy rule can never silently appear permitted in the gate.
    fn from(state: NetworkActionState) -> Self {
        match state.id() {
            "available" => Self::Available,
            "confirmation_required" => Self::ConfirmationRequired,
            _ => Self::Blocked,
        }
    }
}

/// One registry row for the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct NetworkActionView {
    /// Stable action id (`git_fetch`, `mcp_remote_connect`, …).
    pub action: String,
    /// User-facing label.
    pub label: String,
    /// The one situation that triggers the action.
    pub trigger: String,
    /// Whether only an explicit user gesture may run it.
    pub explicit: bool,
    /// Current state.
    pub state: NetworkActionStateView,
    /// User-facing reason when the state is `blocked`.
    pub reason: Option<String>,
}

/// One network event for the webview.
///
/// Exactly four fields on purpose: time, action, host and a byte count. The UI shows them and
/// nothing else, so no response body or header can reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct NetworkEventView {
    /// RFC 3339 timestamp of the attempt.
    pub at: String,
    /// Stable action id that was permitted.
    pub action: String,
    /// Normalized destination host, when the call had one.
    pub host: Option<String>,
    /// Bytes sent and received, or `0` when unknown.
    ///
    /// Sent as a JSON number like every other count in the command surface, not as a `bigint`.
    #[ts(type = "number")]
    pub bytes: u64,
}

/// Everything the network gate and the Settings privacy section need in one payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct NetworkPolicyView {
    /// Offline mode.
    pub offline: bool,
    /// `network.marketplace_enabled`.
    pub marketplace_enabled: bool,
    /// `background_health` as its stable id (`off`, `local` or `all`).
    pub background_health: String,
    /// Whether the user consented to background updates.
    pub updates_opt_in: bool,
    /// Read-only promise shown above the action list, describing the current policy.
    pub statement: String,
    /// Configured host allow-list; empty means any host for a permitted action.
    pub allowed_hosts: Vec<String>,
    /// One row per registered action, in registry order.
    pub actions: Vec<NetworkActionView>,
    /// The last events, oldest first.
    pub events: Vec<NetworkEventView>,
    /// The most events the log can hold.
    pub capacity: u32,
}

/// Builds the policy view from the live state.
#[must_use]
pub fn policy_view(state: &NetworkState) -> NetworkPolicyView {
    let policy = state.policy();
    let actions = policy
        .statuses()
        .into_iter()
        .map(|status| {
            let state = NetworkActionStateView::from(status.state);
            NetworkActionView {
                action: status.action.wire_name().to_owned(),
                label: status.label.to_owned(),
                trigger: status.trigger.to_owned(),
                explicit: status.explicit,
                state,
                reason: status
                    .state
                    .reason()
                    .map(|reason| reason.message().to_owned()),
            }
        })
        .collect();
    NetworkPolicyView {
        offline: policy.offline(),
        marketplace_enabled: policy.marketplace_enabled(),
        background_health: policy.background_health().wire_name().to_owned(),
        updates_opt_in: policy.updates_opt_in(),
        allowed_hosts: policy
            .allowed_hosts()
            .iter()
            .map(|host| host.as_str().to_owned())
            .collect(),
        statement: policy.statement().to_owned(),
        actions,
        events: event_views(&state.events()),
        capacity: NETWORK_EVENT_CAPACITY as u32,
    }
}

/// Builds event views from stored events.
#[must_use]
pub fn event_views(events: &[NetworkEvent]) -> Vec<NetworkEventView> {
    events
        .iter()
        .map(|event| NetworkEventView {
            at: format_at(event.at),
            action: event.action.wire_name().to_owned(),
            host: event.host.as_ref().map(|host| host.as_str().to_owned()),
            bytes: event.bytes,
        })
        .collect()
}

fn format_at(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

/// Returns the current policy and the last events.
#[tauri::command]
pub fn network_policy(state: State<'_, NetworkState>) -> NetworkPolicyView {
    policy_view(&state)
}

/// Returns the stored network events, oldest first.
#[tauri::command]
pub fn network_events(state: State<'_, NetworkState>) -> Vec<NetworkEventView> {
    event_views(&state.events())
}

/// Clears the event log — the Settings "Clear" button — and returns the empty log.
#[tauri::command]
pub fn network_clear_events(state: State<'_, NetworkState>) -> NetworkPolicyView {
    state.clear_events();
    policy_view(&state)
}

/// Turns offline mode on or off and returns the new state.
#[tauri::command]
pub fn network_set_offline(state: State<'_, NetworkState>, offline: bool) -> NetworkPolicyView {
    state.set_offline(offline);
    policy_view(&state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::network::DenialReason;

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn event(action: NetworkAction, offset_seconds: i64) -> NetworkEvent {
        NetworkEvent::new(
            NOW + time::Duration::seconds(offset_seconds),
            action,
            NetworkHost::parse("mcp.example.test").ok(),
            12,
        )
    }

    #[test]
    fn the_default_state_permits_no_background_action() {
        let state = NetworkState::default();
        for &action in NetworkAction::ALL {
            let denied = state
                .authorize(action, "example.test", NetworkTrigger::Background)
                .expect_err("no background action is permitted by default");
            assert!(
                matches!(
                    denied.reason,
                    DenialReason::ActionDisabled | DenialReason::BackgroundNotConsented
                ),
                "{action}: {:?}",
                denied.reason
            );
        }
    }

    #[test]
    fn the_policy_view_lists_every_action_with_its_state_and_reason() {
        let state = NetworkState::default();
        let view = policy_view(&state);
        assert_eq!(view.actions.len(), 11);
        assert_eq!(view.capacity, 20);
        assert!(!view.offline);
        assert!(!view.marketplace_enabled);
        assert_eq!(view.background_health, "local");
        assert!(!view.updates_opt_in);
        assert!(view.allowed_hosts.is_empty());
        assert!(view.statement.contains("no telemetry"));
        assert!(view.events.is_empty());

        let scheduled = view
            .actions
            .iter()
            .find(|row| row.action == "scheduled_http_health_check")
            .expect("the scheduled action is registered");
        assert_eq!(scheduled.state, NetworkActionStateView::Blocked);
        assert_eq!(
            scheduled.reason.as_deref(),
            Some(DenialReason::ActionDisabled.message())
        );
        assert!(!scheduled.explicit);

        let fetch = view
            .actions
            .iter()
            .find(|row| row.action == "git_fetch")
            .expect("git fetch is registered");
        assert_eq!(fetch.state, NetworkActionStateView::ConfirmationRequired);
        assert_eq!(fetch.reason, None);
        assert!(fetch.explicit);
        assert!(!fetch.trigger.is_empty());
    }

    #[test]
    fn the_ring_buffer_keeps_twenty_and_the_clear_button_empties_it() {
        let state = NetworkState::default();
        for index in 0..25 {
            state.record(event(NetworkAction::GitFetch, index));
        }
        assert_eq!(state.event_count(), 20);

        let view = policy_view(&state);
        assert_eq!(view.events.len(), 20);
        assert_eq!(
            view.events.first().expect("oldest kept").at,
            "1970-01-01T00:00:05Z"
        );
        assert_eq!(
            view.events.last().expect("newest").at,
            "1970-01-01T00:00:24Z"
        );

        state.clear_events();
        assert_eq!(state.event_count(), 0);
        assert!(policy_view(&state).events.is_empty());
    }

    #[test]
    fn event_views_expose_only_time_action_host_and_bytes() {
        let view = event_views(&[event(NetworkAction::McpRemoteConnect, 0)]);
        let json = serde_json::to_value(&view[0]).expect("serializes");
        let object = json.as_object().expect("an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["action", "at", "bytes", "host"]);
        assert_eq!(json["action"], "mcp_remote_connect");
        assert_eq!(json["host"], "mcp.example.test");
        assert_eq!(json["at"], "1970-01-01T00:00:00Z");
        assert_eq!(json["bytes"], 12);
    }

    #[test]
    fn the_update_gate_needs_a_user_or_an_opt_in() {
        let policy = NetworkPolicy::default();
        assert!(
            authorize_update(
                &policy,
                NetworkAction::UpdateCheck,
                "updates.example.test",
                NetworkTrigger::User
            )
            .is_ok()
        );
        let denied = authorize_update(
            &policy,
            NetworkAction::UpdateDownload,
            "updates.example.test",
            NetworkTrigger::Background,
        )
        .expect_err("background updates are off by default");
        assert_eq!(
            denied.denied().expect("a policy refusal").reason,
            DenialReason::BackgroundNotConsented
        );

        let opted_in = policy.with_updates_opt_in(true);
        assert!(
            authorize_update(
                &opted_in,
                NetworkAction::UpdateCheck,
                "updates.example.test",
                NetworkTrigger::Background
            )
            .is_ok()
        );
    }

    #[test]
    fn the_update_gate_refuses_actions_it_does_not_own() {
        let policy = NetworkPolicy::default().with_offline(true);
        let error = authorize_update(
            &policy,
            NetworkAction::GitFetch,
            "github.com",
            NetworkTrigger::User,
        )
        .expect_err("git is not an update action");
        assert!(matches!(
            error,
            UpdateGateError::NotAnUpdateAction(NetworkAction::GitFetch)
        ));
        assert!(error.denied().is_none());
    }

    #[test]
    fn the_offline_switch_blocks_and_restores_actions() {
        let state = NetworkState::default();
        state.set_offline(true);
        for &action in NetworkAction::ALL {
            let denied = state
                .authorize(action, "example.test", NetworkTrigger::User)
                .expect_err("offline blocks everything");
            assert_eq!(denied.reason, DenialReason::Offline, "{action}");
        }
        assert!(policy_view(&state).offline);

        state.set_offline(false);
        assert!(
            state
                .authorize(NetworkAction::GitFetch, "github.com", NetworkTrigger::User)
                .is_ok()
        );
    }

    #[test]
    fn settings_updates_reach_the_policy_view() {
        let state = NetworkState::default();
        state.set_marketplace_enabled(true);
        state.set_background_health(at_model::network::BackgroundHealth::All);
        state.set_updates_opt_in(true);
        let view = policy_view(&state);
        assert!(view.marketplace_enabled);
        assert_eq!(view.background_health, "all");
        assert!(view.updates_opt_in);
        let scheduled = view
            .actions
            .iter()
            .find(|row| row.action == "scheduled_http_health_check")
            .expect("registered");
        assert_eq!(scheduled.state, NetworkActionStateView::Available);
    }

    #[test]
    fn the_log_is_bounded_by_the_shared_constant() {
        assert_eq!(NETWORK_EVENT_CAPACITY, NetworkEventLog::CAPACITY);
        assert_eq!(NetworkState::default().event_count(), 0);
    }
}
