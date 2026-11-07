//! OS deep links: `agentuna://item/<item-id>`.
//!
//! macOS hands an opened URL to the running app through [`tauri::RunEvent::Opened`]; the scheme
//! itself is registered in the bundle's `Info.plist` (`CFBundleURLTypes`). A cold start delivers
//! the URL before the webview has subscribed, so links that arrive first wait in
//! [`DeepLinkRelay`] until the frontend's first `deep_link_take` call.
//!
//! Nothing here trusts the incoming string. Every URL is parsed into the typed
//! [`ItemId`] and re-serialized, so the webview only ever receives a canonical link that its own
//! parser accepts; anything else is counted and dropped without reaching the webview.

use std::sync::{Mutex, MutexGuard, PoisonError};

use at_model::item::ItemId;
use tauri::Emitter;

/// Event the webview listens for, carrying one canonical link per payload.
pub const DEEP_LINK_EVENT: &str = "deep-link";

/// Links kept while the webview is still loading. macOS sends one URL per open request; the cap
/// only bounds a burst.
const PENDING_CAPACITY: usize = 8;

/// Parses `agentuna://item/<item-id>` and returns the typed id.
///
/// Rejects another scheme or host, credentials, ports, queries, fragments, extra path segments
/// and ids that are not canonical [`ItemId`]s (the same shape the webview regex accepts, plus the
/// id's own version and canonical-form checks).
#[must_use]
pub fn parse_item_link(url: &url::Url) -> Option<ItemId> {
    if url.scheme() != "agentuna" || url.host_str() != Some("item") {
        return None;
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segment = url.path().strip_prefix('/')?;
    if segment.is_empty() || segment.contains('/') {
        return None;
    }
    segment.parse::<ItemId>().ok()
}

/// The canonical link for `id`; the only shape emitted to the webview.
#[must_use]
pub fn canonical_link(id: ItemId) -> String {
    format!("agentuna://item/{id}")
}

/// Links waiting for the webview to announce that it listens.
///
/// The first `deep_link_take` flips `listening`; after that links are emitted immediately. A
/// poisoned lock is recovered rather than propagated: a stale lock must not stop link delivery.
#[derive(Default)]
pub struct DeepLinkRelay {
    links: Mutex<RelayState>,
}

#[derive(Default)]
struct RelayState {
    listening: bool,
    pending: Vec<String>,
}

impl DeepLinkRelay {
    fn lock(&self) -> MutexGuard<'_, RelayState> {
        self.links.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Validates `urls` and delivers the canonical links. Unsupported URLs are counted, never
    /// logged by value: an OS URL can carry more than the item id.
    pub fn deliver(&self, app: &tauri::AppHandle, urls: &[url::Url]) {
        let links: Vec<String> = urls
            .iter()
            .filter_map(parse_item_link)
            .map(canonical_link)
            .collect();
        if links.len() != urls.len() {
            tracing::warn!(
                rejected = urls.len() - links.len(),
                accepted = links.len(),
                "ignored an unsupported deep link"
            );
        }
        if let Some(links) = self.accept(links) {
            for link in links {
                emit(app, &link);
            }
        }
    }

    /// Queues `links` while the webview is loading; returns them for emission when it listens.
    fn accept(&self, links: Vec<String>) -> Option<Vec<String>> {
        if links.is_empty() {
            return Some(links);
        }
        let mut state = self.lock();
        if state.listening {
            return Some(links);
        }
        for link in links {
            if state.pending.len() == PENDING_CAPACITY {
                state.pending.remove(0);
            }
            state.pending.push(link);
        }
        None
    }

    /// Marks the webview as listening and returns the links it missed.
    #[must_use]
    pub fn take(&self) -> Vec<String> {
        let mut state = self.lock();
        state.listening = true;
        core::mem::take(&mut state.pending)
    }
}

fn emit(app: &tauri::AppHandle, link: &str) {
    if let Err(error) = app.emit(DEEP_LINK_EVENT, link) {
        tracing::warn!(%error, "deep link emit failed");
    }
}

/// Returns the canonical item links that arrived before the webview could listen.
///
/// The frontend calls this once on mount; every later link arrives through the `deep-link`
/// event. Taking twice returns an empty list.
#[tauri::command]
pub fn deep_link_take(relay: tauri::State<'_, DeepLinkRelay>) -> Vec<String> {
    relay.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ItemId {
        "itm_019a0000-0000-7000-8000-000000000000"
            .parse()
            .expect("the fixture is a canonical item id")
    }

    fn parse(raw: &str) -> Option<ItemId> {
        url::Url::parse(raw)
            .ok()
            .and_then(|url| parse_item_link(&url))
    }

    #[test]
    fn canonical_links_round_trip_through_the_parser() {
        let id = id();
        assert_eq!(parse(&canonical_link(id)), Some(id));
        assert_eq!(canonical_link(id), format!("agentuna://item/{id}"));
    }

    #[test]
    fn other_schemes_hosts_and_shapes_are_rejected() {
        let id = id();
        for raw in [
            format!("https://item/{id}"),
            format!("agentuna://other/{id}"),
            format!("agentuna://item/{id}/extra"),
            "agentuna://item/".to_owned(),
            "agentuna:///item/itm_019a0000-0000-7000-8000-000000000000".to_owned(),
            format!("agentuna://user@item/{id}"),
            format!("agentuna://item:80/{id}"),
            format!("agentuna://item/{id}?ignored=true"),
            format!("agentuna://item/{id}#fragment"),
        ] {
            assert_eq!(parse(&raw), None, "{raw} must be rejected");
        }
    }

    #[test]
    fn malformed_and_non_canonical_item_ids_are_rejected() {
        for raw in [
            "agentuna://item/itm_linked",
            "agentuna://item/019a0000-0000-7000-8000-000000000000",
            "agentuna://item/itm_019A0000-0000-7000-8000-000000000000",
            "agentuna://item/itm_019a0000000070008000000000000000",
            "agentuna://item/itm_019a0000-0000-4000-8000-000000000000",
            "agentuna://item/itm_019a0000-0000-7000-8000-00000000000g",
        ] {
            assert_eq!(parse(raw), None, "{raw} must be rejected");
        }
    }

    #[test]
    fn links_wait_for_the_first_take_then_arrive_directly() {
        let relay = DeepLinkRelay::default();
        let first = canonical_link(id());
        assert_eq!(relay.accept(vec![first.clone()]), None);
        assert_eq!(relay.take(), vec![first.clone()]);
        assert_eq!(relay.take(), Vec::<String>::new());
        assert_eq!(relay.accept(vec![first.clone()]), Some(vec![first]));
        assert_eq!(relay.accept(Vec::new()), Some(Vec::new()));
    }

    #[test]
    fn the_pending_buffer_is_bounded_and_drops_the_oldest() {
        let relay = DeepLinkRelay::default();
        let links: Vec<String> = (0..(PENDING_CAPACITY + 2))
            .map(|_| canonical_link(id()))
            .collect();
        assert_eq!(relay.accept(links.clone()), None);
        let taken = relay.take();
        assert_eq!(taken.len(), PENDING_CAPACITY);
        assert_eq!(taken.as_slice(), &links[2..]);
    }
}
