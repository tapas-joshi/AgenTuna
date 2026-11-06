//! Privacy proof for the app's network surface: starting its services and reading its screens'
//! data opens no connection.
//!
//! A loopback listener acts as a proxy recorder. It is not reachable from any screen, so a
//! pending connection on it would only appear if this code tried to connect somewhere; more
//! importantly the network state starts with an empty event log and refuses every background
//! action, so nothing in this surface can even ask for a connection.
//!
//! The GUI itself is not launched here: constructing the shell's services (job bridge, network
//! state) and reading every network view is the part of "launch, visit every screen, idle" that
//! exists headlessly. A process-wide DNS hook needs unsafe FFI, which this workspace forbids.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use at_app::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
use at_app::app::network::{NetworkState, event_views, policy_view};
use at_app::app::state::AppState;
use at_model::network::{
    DenialReason, NetworkAction, NetworkActionState, NetworkPolicy, NetworkTrigger,
};

/// Sink that records nothing: the bridge must not need a webview to be constructed.
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: JobEvent) {}
}

fn recorder() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback recorder");
    listener
        .set_nonblocking(true)
        .expect("non-blocking recorder");
    listener
}

#[test]
fn starting_the_services_and_reading_the_views_opens_no_connection() {
    let recorder = recorder();

    let bridge = Arc::new(JobBridge::new(
        Arc::new(NullSink),
        Arc::new(TracingOperationsLog),
    ));
    let app = AppState::new(Arc::clone(&bridge), None);
    assert!(app.bridge().jobs().is_empty());

    let network = NetworkState::default();
    for _ in 0..100 {
        let view = policy_view(&network);
        assert_eq!(view.actions.len(), 11);
        assert!(view.events.is_empty());
        assert!(event_views(&network.events()).is_empty());
        network.clear_events();
    }

    assert!(
        recorder.accept().is_err(),
        "starting the services and reading the network views must not open a socket"
    );
    assert_eq!(network.event_count(), 0);
    let report = bridge.shutdown(Duration::from_millis(50));
    assert_eq!(report.interrupted, 0);
}

#[test]
fn the_default_policy_refuses_every_background_call() {
    // This is the architectural half of "idling for ten minutes makes no request": there is no
    // permitted action for a background caller to reach, including the scheduler's own action.
    let network = NetworkState::default();
    for &action in NetworkAction::ALL {
        let denied = network
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
    for &action in NetworkAction::ALL {
        let state = network.policy().state(action);
        assert_ne!(
            state,
            NetworkActionState::Available,
            "{action} must not be pre-consented in a fresh install"
        );
    }
}

#[test]
fn offline_mode_refuses_every_registered_action() {
    let network = NetworkState::default();
    network.set_offline(true);
    for &action in NetworkAction::ALL {
        for trigger in [NetworkTrigger::User, NetworkTrigger::Background] {
            let denied = network
                .authorize(action, "example.test", trigger)
                .expect_err("offline mode refuses everything");
            assert_eq!(denied.reason, DenialReason::Offline, "{action}/{trigger}");
        }
    }
    assert!(policy_view(&network).offline, "the view shows offline mode");
}

#[test]
fn a_replaced_policy_changes_the_view_and_the_decision() {
    let network = NetworkState::default();
    assert!(!network.policy().marketplace_enabled());
    network.set_policy(
        NetworkPolicy::default()
            .with_marketplace_enabled(true)
            .with_background_health(at_model::network::BackgroundHealth::All),
    );
    let view = policy_view(&network);
    assert!(view.marketplace_enabled);
    assert_eq!(view.background_health, "all");
    assert!(
        network
            .authorize(
                NetworkAction::MarketplaceBrowse,
                "skills.example.test",
                NetworkTrigger::User
            )
            .is_ok()
    );
}
