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

/// Why a peer is not currently [`Connected`](PeerConnState::Connected) — a purely
/// **diagnostic** refinement of [`PeerConnState::Offline`] for PRD §16.3 / §18.1
/// output (spec §4.5). It is *never* a trust input: admission and the dial set are
/// keyed only by the QUIC-proven `device_id`, and this reason only refines the
/// human-facing label the CLI renders for an offline peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OfflineReason {
    /// Desired, but no dial attempt has completed yet (the transient default an
    /// entry carries until a dial resolves).
    #[default]
    NeverDialed,
    /// `endpoint.connect()` failed — no path to the peer (the common "peer is
    /// offline / unreachable" case).
    Unreachable,
    /// Connected at QUIC but the stream open / handshake failed (TLS/ALPN/proto);
    /// reachable, but the transport could not carry events.
    TransportError,
    /// Was [`Connected`](PeerConnState::Connected) and the live link fell — a
    /// transient drop the dial loop will redial.
    LinkDropped,
    /// Removed from the room mid-session; the managed dial loop was stopped
    /// (terminal — we will not redial a since-removed peer).
    Deauthorized,
}

impl OfflineReason {
    /// Stable lowercase label for logs/audit/CLI output (pinned by tests exactly
    /// like [`PeerConnState::label`]; tooling parses these strings).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NeverDialed => "never_dialed",
            Self::Unreachable => "unreachable",
            Self::TransportError => "transport_error",
            Self::LinkDropped => "link_dropped",
            Self::Deauthorized => "deauthorized",
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
    /// Why the peer is offline, when [`state`](Self::state) is
    /// [`Offline`](PeerConnState::Offline). Ignored (and left at its last value)
    /// for the other states — read it only alongside an `Offline` state.
    pub offline_reason: OfflineReason,
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
    ///
    /// This never *changes* the [`OfflineReason`]; use [`set_offline`](Self::set_offline)
    /// to move a peer to [`Offline`](PeerConnState::Offline) with a diagnostic reason.
    pub fn set(&self, device: EndpointId, state: PeerConnState, identity: Option<IdentityKey>) {
        self.set_inner(device, state, identity, None);
    }

    /// Move a device to [`Offline`](PeerConnState::Offline) carrying a diagnostic
    /// [`OfflineReason`] (spec §4.5). The reason refines the offline label for the
    /// CLI/logs; it is not a trust input.
    ///
    /// A genuine state transition into `Offline` emits a [`ConnEvent`]. A *reason*
    /// refinement while already `Offline` updates the stored reason but — per spec
    /// D5 — emits **no** new `ConnEvent`, keeping the transition stream a pure
    /// state-change stream (the CLI reads the reason from the entry when it renders
    /// a transition).
    pub fn set_offline(
        &self,
        device: EndpointId,
        reason: OfflineReason,
        identity: Option<IdentityKey>,
    ) {
        self.set_inner(device, PeerConnState::Offline, identity, Some(reason));
    }

    fn set_inner(
        &self,
        device: EndpointId,
        state: PeerConnState,
        identity: Option<IdentityKey>,
        reason: Option<OfflineReason>,
    ) {
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
                            offline_reason: reason.unwrap_or_default(),
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
                        // Idempotent re-set. A reason refinement (still Offline) updates
                        // the stored reason silently (D5: no new transition event).
                        if let Some(r) = reason {
                            if entry.offline_reason != r {
                                entry.offline_reason = r;
                                entry.last_change_ms = ts_ms;
                            }
                        }
                        None
                    } else {
                        let from = entry.state;
                        entry.state = state;
                        if let Some(r) = reason {
                            entry.offline_reason = r;
                        }
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

    /// A point-in-time snapshot of every known device and its full [`PeerEntry`]
    /// (state **and** offline reason **and** bound identity) — the entry the CLI
    /// renders for the §16.3 connection panel. [`snapshot`](Self::snapshot) stays
    /// for state-only back-compat.
    #[must_use]
    pub fn entries(&self) -> Vec<(EndpointId, PeerEntry)> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.iter().map(|(k, v)| (*k, *v)).collect()
    }

    /// The membership identity bound to a device, if it has been learned (set on a
    /// successful admit). Keyed by **endpoint** (`device_id`); the identity is the
    /// secondary grouping key the CLI shows (§4.6).
    #[must_use]
    pub fn identity_of(&self, device: EndpointId) -> Option<IdentityKey> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&device).and_then(|e| e.identity)
    }

    /// Every device currently bound to `identity` — the reverse of
    /// [`identity_of`](Self::identity_of), so the CLI can group a member's multiple
    /// devices under one identity row (§4.6).
    #[must_use]
    pub fn devices_of(&self, identity: IdentityKey) -> Vec<EndpointId> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .filter(|(_, v)| v.identity == Some(identity))
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
    use super::{OfflineReason, PeerConnState, PeerTable};
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

    #[test]
    fn offline_reason_labels_are_stable() {
        // The §16.3 diagnostic vocabulary — pinned exactly like the state labels
        // since the CLI panel / audit lines render them and tooling greps them.
        assert_eq!(OfflineReason::NeverDialed.label(), "never_dialed");
        assert_eq!(OfflineReason::Unreachable.label(), "unreachable");
        assert_eq!(OfflineReason::TransportError.label(), "transport_error");
        assert_eq!(OfflineReason::LinkDropped.label(), "link_dropped");
        assert_eq!(OfflineReason::Deauthorized.label(), "deauthorized");
    }

    #[test]
    fn offline_reason_default_is_never_dialed() {
        assert_eq!(OfflineReason::default(), OfflineReason::NeverDialed);
    }

    #[test]
    fn set_offline_records_reason_and_emits_transition_once() {
        let table = PeerTable::new(16);
        let mut rx = table.subscribe();

        // Connecting -> Offline{Unreachable} is a genuine transition: one event.
        table.set(device(1), PeerConnState::Connecting, None);
        let _ = rx.try_recv().expect("first-sight connecting");
        table.set_offline(device(1), OfflineReason::Unreachable, None);
        let ev = rx.try_recv().expect("transition into offline");
        assert_eq!(ev.from, PeerConnState::Connecting);
        assert_eq!(ev.to, PeerConnState::Offline);

        let entry = table
            .entries()
            .into_iter()
            .find(|(d, _)| *d == device(1))
            .map(|(_, e)| e)
            .expect("entry");
        assert_eq!(entry.offline_reason, OfflineReason::Unreachable);
    }

    #[test]
    fn set_offline_reason_refinement_updates_entry_without_new_event() {
        let table = PeerTable::new(16);
        table.set_offline(device(2), OfflineReason::Unreachable, None);
        let mut rx = table.subscribe(); // subscribe AFTER first sight

        // Refine the reason while staying Offline: entry updates, but no event (D5).
        table.set_offline(device(2), OfflineReason::LinkDropped, None);
        assert!(
            rx.try_recv().is_err(),
            "a reason-only refinement must not emit a ConnEvent"
        );
        let entry = table
            .entries()
            .into_iter()
            .find(|(d, _)| *d == device(2))
            .map(|(_, e)| e)
            .expect("entry");
        assert_eq!(entry.offline_reason, OfflineReason::LinkDropped);
    }

    #[test]
    fn identity_and_devices_reverse_map() {
        use iroh_rooms_core::event::keys::IdentityKey;
        let table = PeerTable::new(16);
        let id = IdentityKey::from_bytes([0xAB; 32]);
        table.set(device(3), PeerConnState::Connected, Some(id));
        table.set(device(4), PeerConnState::Connected, Some(id));
        table.set(device(5), PeerConnState::Connected, None);

        assert_eq!(table.identity_of(device(3)), Some(id));
        assert_eq!(table.identity_of(device(5)), None);
        let mut devs = table.devices_of(id);
        devs.sort();
        let mut want = vec![device(3), device(4)];
        want.sort();
        assert_eq!(devs, want);
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
