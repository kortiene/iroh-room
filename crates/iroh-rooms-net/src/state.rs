//! The observable per-peer connection-state model (PRD §16.3; spec §4.5 / G4).
//!
//! The whole point of PRD §16.3 is that the app can tell an **offline** member
//! (authorized, no path right now) apart from an **unauthorized** peer (one we
//! will never talk to, regardless of reachability). [`PeerConnState`] encodes
//! exactly that distinction plus a transient dial state, and the shared
//! [`PeerTable`] surfaces it two ways: a point-in-time [`PeerTable::snapshot`] and
//! a live [`ConnEvent`] change stream (`tokio::sync::broadcast`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use iroh::EndpointId;
use iroh_rooms_core::event::keys::IdentityKey;
use tokio::sync::broadcast;

/// Per-peer connection state surfaced to the CLI/app (PRD §16.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerConnState {
    /// A dial is in progress (transient; not part of the AC trio but useful for
    /// the reconnect timeline).
    Connecting,
    /// Authenticated Active member with a live bidi stream up.
    Connected,
    /// A member we expect, but with no live connection right now (dial failing /
    /// unreachable / link dropped). *Authorized* — we will keep redialing.
    Offline,
    /// A device that presented itself but is **not** a bound Active member, so it
    /// was rejected. Never holds a live stream; we will not talk to it regardless
    /// of reachability.
    Unauthorized,
}

impl PeerConnState {
    /// Stable lowercase label for logs/audit/CLI output.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Offline => "offline",
            Self::Unauthorized => "unauthorized",
        }
    }
}

/// One peer's tracked entry in the [`PeerTable`].
#[derive(Clone, Copy, Debug)]
pub struct PeerEntry {
    /// The current connection state.
    pub state: PeerConnState,
    /// The membership identity the device is bound to, once known (set on admit).
    pub identity: Option<IdentityKey>,
    /// Wall-clock ms of the last state change (observability only; not a trust
    /// input).
    pub last_change_ms: u64,
}

/// A connection-state transition, broadcast to live observers (CLI `room
/// members`, the audit sink, the engine driver).
#[derive(Clone, Copy, Debug)]
pub struct ConnEvent {
    /// The device whose state changed.
    pub device: EndpointId,
    /// The state it moved from (equal to `to` only for the very first observation
    /// of a device, which still emits so late subscribers learn the device).
    pub from: PeerConnState,
    /// The state it moved to.
    pub to: PeerConnState,
    /// Wall-clock ms of the transition.
    pub ts_ms: u64,
}

/// Shared, cheaply-cloneable per-peer state table with a change-event stream.
///
/// Cloning shares the same underlying map and broadcast channel, so the accept
/// handler, the per-peer tasks, and the public [`NetTransport`](crate::NetTransport)
/// surface all observe and mutate one consistent view.
#[derive(Clone)]
pub struct PeerTable {
    inner: Arc<Mutex<HashMap<EndpointId, PeerEntry>>>,
    events: broadcast::Sender<ConnEvent>,
}

impl PeerTable {
    /// Create an empty table. `event_capacity` bounds the broadcast backlog a slow
    /// observer may lag by before it starts losing the oldest transitions.
    #[must_use]
    pub fn new(event_capacity: usize) -> Self {
        let (events, _rx) = broadcast::channel(event_capacity.max(1));
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            events,
        }
    }

    /// Subscribe to the live transition stream.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ConnEvent> {
        self.events.subscribe()
    }

    /// Set a device's state, optionally recording its bound identity.
    ///
    /// Emits a [`ConnEvent`] on the **first sight** of a device (with `from ==
    /// to`, so a late subscriber and the inbound-accept path both learn it) and on
    /// every genuine transition thereafter; an idempotent re-set to the same state
    /// is a silent no-op (so the engine driver's `on_connect`/`on_disconnect` fire
    /// exactly once per real transition even when both the dial and accept sides
    /// race to `Connected`). The `identity` (when `Some`) is always recorded.
    pub fn set(&self, device: EndpointId, state: PeerConnState, identity: Option<IdentityKey>) {
        let ts_ms = now_ms();
        let event = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match guard.get_mut(&device) {
                None => {
                    guard.insert(
                        device,
                        PeerEntry {
                            state,
                            identity,
                            last_change_ms: ts_ms,
                        },
                    );
                    // First sight: emit `from == to` so observers register the device.
                    Some(ConnEvent {
                        device,
                        from: state,
                        to: state,
                        ts_ms,
                    })
                }
                Some(entry) => {
                    if let Some(id) = identity {
                        entry.identity = Some(id);
                    }
                    if entry.state == state {
                        // Idempotent re-set: record identity (done above) but emit nothing.
                        None
                    } else {
                        let from = entry.state;
                        entry.state = state;
                        entry.last_change_ms = ts_ms;
                        Some(ConnEvent {
                            device,
                            from,
                            to: state,
                            ts_ms,
                        })
                    }
                }
            }
        };

        if let Some(event) = event {
            let _ = self.events.send(event);
        }
    }

    /// A point-in-time snapshot of every known device and its current state.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(EndpointId, PeerConnState)> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.iter().map(|(k, v)| (*k, v.state)).collect()
    }

    /// The current state of one device, if known.
    #[must_use]
    pub fn state_of(&self, device: EndpointId) -> Option<PeerConnState> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&device).map(|e| e.state)
    }

    /// The devices currently in [`PeerConnState::Connected`] (the engine's
    /// authenticated fan-out set).
    #[must_use]
    pub fn connected_devices(&self) -> Vec<EndpointId> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .filter(|(_, v)| v.state == PeerConnState::Connected)
            .map(|(k, _)| *k)
            .collect()
    }
}

/// Wall-clock milliseconds since the Unix epoch (observability timestamps only).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::{PeerConnState, PeerTable};
    use iroh::{EndpointId, SecretKey};

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn first_sight_then_transition_emit_events() {
        let table = PeerTable::new(16);
        let mut rx = table.subscribe();

        table.set(device(1), PeerConnState::Connecting, None);
        let first = rx.try_recv().expect("first-sight event");
        assert_eq!(first.from, PeerConnState::Connecting);
        assert_eq!(first.to, PeerConnState::Connecting);

        table.set(device(1), PeerConnState::Connected, None);
        let second = rx.try_recv().expect("transition event");
        assert_eq!(second.from, PeerConnState::Connecting);
        assert_eq!(second.to, PeerConnState::Connected);
    }

    #[test]
    fn idempotent_reset_emits_no_event() {
        let table = PeerTable::new(16);
        table.set(device(1), PeerConnState::Connected, None);
        let mut rx = table.subscribe(); // subscribe AFTER first sight

        // Re-setting the same state must be a silent no-op.
        table.set(device(1), PeerConnState::Connected, None);
        assert!(
            rx.try_recv().is_err(),
            "idempotent re-set must not emit a ConnEvent"
        );
    }

    #[test]
    fn snapshot_and_connected_devices_reflect_state() {
        let table = PeerTable::new(16);
        table.set(device(1), PeerConnState::Connected, None);
        table.set(device(2), PeerConnState::Offline, None);
        table.set(device(3), PeerConnState::Unauthorized, None);

        assert_eq!(table.state_of(device(1)), Some(PeerConnState::Connected));
        assert_eq!(table.connected_devices(), vec![device(1)]);
        assert_eq!(table.snapshot().len(), 3);
    }

    // --- Stable label strings ---

    #[test]
    fn peer_conn_state_labels_are_stable() {
        // These labels appear in logs, CLI output, and audit (PRD §16.3). A change
        // silently breaks tooling that parses them. Pin them explicitly.
        assert_eq!(PeerConnState::Connecting.label(), "connecting");
        assert_eq!(PeerConnState::Connected.label(), "connected");
        assert_eq!(PeerConnState::Offline.label(), "offline");
        assert_eq!(PeerConnState::Unauthorized.label(), "unauthorized");
    }

    // --- Unknown-device lookups ---

    #[test]
    fn state_of_unknown_device_returns_none() {
        let table = PeerTable::new(8);
        assert!(table.state_of(device(99)).is_none());
    }

    // --- connected_devices filtering ---

    #[test]
    fn connected_devices_empty_on_fresh_table() {
        let table = PeerTable::new(8);
        assert!(table.connected_devices().is_empty());
    }

    #[test]
    fn connected_devices_excludes_offline_and_unauthorized() {
        let table = PeerTable::new(8);
        table.set(device(1), PeerConnState::Connected, None);
        table.set(device(2), PeerConnState::Offline, None);
        table.set(device(3), PeerConnState::Unauthorized, None);
        table.set(device(4), PeerConnState::Connecting, None);

        let connected = table.connected_devices();
        assert_eq!(
            connected.len(),
            1,
            "only the Connected device should appear"
        );
        assert_eq!(connected[0], device(1));
    }

    // --- Full transition sequence ---

    #[test]
    fn full_transition_sequence_updates_state_of() {
        let table = PeerTable::new(8);
        table.set(device(1), PeerConnState::Connecting, None);
        assert_eq!(table.state_of(device(1)), Some(PeerConnState::Connecting));

        table.set(device(1), PeerConnState::Connected, None);
        assert_eq!(table.state_of(device(1)), Some(PeerConnState::Connected));

        table.set(device(1), PeerConnState::Offline, None);
        assert_eq!(table.state_of(device(1)), Some(PeerConnState::Offline));
    }

    // --- Multiple independent devices ---

    #[test]
    fn multiple_devices_tracked_independently() {
        let table = PeerTable::new(16);
        table.set(device(1), PeerConnState::Connected, None);
        table.set(device(2), PeerConnState::Offline, None);

        assert_eq!(table.state_of(device(1)), Some(PeerConnState::Connected));
        assert_eq!(table.state_of(device(2)), Some(PeerConnState::Offline));

        // Transition one device; the other must be unaffected.
        table.set(device(1), PeerConnState::Offline, None);
        assert_eq!(table.state_of(device(1)), Some(PeerConnState::Offline));
        assert_eq!(table.state_of(device(2)), Some(PeerConnState::Offline));
    }

    // --- Emit ConnEvent on first sight (from == to) ---

    #[test]
    fn first_sight_event_has_from_equal_to() {
        let table = PeerTable::new(8);
        let mut rx = table.subscribe();

        table.set(device(5), PeerConnState::Unauthorized, None);
        let ev = rx.try_recv().expect("first-sight event for Unauthorized");
        assert_eq!(ev.from, PeerConnState::Unauthorized);
        assert_eq!(ev.to, PeerConnState::Unauthorized);
        assert_eq!(ev.device, device(5));
    }

    // --- Clone shares the same underlying Arc/Mutex ---

    #[test]
    fn cloned_table_observes_writes_through_original() {
        let table = PeerTable::new(8);
        let clone = table.clone();

        table.set(device(10), PeerConnState::Connected, None);
        assert_eq!(
            clone.state_of(device(10)),
            Some(PeerConnState::Connected),
            "a cloned PeerTable shares the same Arc<Mutex<...>> as the original"
        );
    }

    #[test]
    fn original_observes_writes_through_clone() {
        let table = PeerTable::new(8);
        let clone = table.clone();

        clone.set(device(11), PeerConnState::Offline, None);
        assert_eq!(table.state_of(device(11)), Some(PeerConnState::Offline));
    }

    #[test]
    fn subscriber_on_original_sees_events_from_cloned_writer() {
        let table = PeerTable::new(8);
        let mut rx = table.subscribe();
        let clone = table.clone();

        // Write through the clone — the broadcast channel is shared.
        clone.set(device(12), PeerConnState::Unauthorized, None);
        let ev = rx
            .try_recv()
            .expect("first-sight event must arrive via the shared broadcast channel");
        assert_eq!(ev.device, device(12));
        assert_eq!(ev.to, PeerConnState::Unauthorized);
    }

    // --- ConnEvent timestamp is a non-zero u64 ---

    #[test]
    fn conn_event_timestamp_is_positive() {
        let table = PeerTable::new(8);
        let mut rx = table.subscribe();
        table.set(device(20), PeerConnState::Connected, None);
        let ev = rx.try_recv().expect("event");
        // The timestamp is wall-clock ms (not zero on any modern system).
        assert!(ev.ts_ms > 0, "conn event timestamp must be positive");
    }

    // --- snapshot ordering is stable under multiple states ---

    #[test]
    fn snapshot_length_grows_with_each_new_device() {
        let table = PeerTable::new(8);
        assert_eq!(table.snapshot().len(), 0);
        table.set(device(30), PeerConnState::Connected, None);
        assert_eq!(table.snapshot().len(), 1);
        table.set(device(31), PeerConnState::Offline, None);
        assert_eq!(table.snapshot().len(), 2);
        // Re-setting an existing device does not add a new entry.
        table.set(device(30), PeerConnState::Offline, None);
        assert_eq!(table.snapshot().len(), 2);
    }
}
