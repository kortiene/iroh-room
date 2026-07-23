//! [`PeerManager`] — the room-scoped owner of the outbound dial set
//! (`PHASE-0-SPIKE.md` ADR-1 "per-room peer manager"; spec §4.1 / §4.2 / G1).
//!
//! IR-0005 landed the pieces — a per-peer [`dial_loop`](crate::peer::dial_loop)
//! with bounded backoff, the [`PeerTable`](crate::state::PeerTable) connection-state
//! model, and the accept gate — but nothing *owned the set of connections a room
//! should maintain* and kept it correct as membership changed. The manager closes
//! that gap: it derives the **desired** outbound set from the live membership
//! snapshot (active members' devices minus self) and **reconciles** the running
//! dial-loop set against it — starting loops for newly-active members, aborting and
//! tearing down loops for since-removed ones.
//!
//! It **composes** the landed primitives (spec D1/R1): it does not re-implement the
//! dial loop, the backoff schedule, the frame codec, or the accept gate. It owns
//! only the `device → running loop` map and drives start/stop against it.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_rooms_core::membership::MembershipSnapshot;
#[cfg(feature = "gossip_overlay")]
use iroh_rooms_core::sync::{Outgoing, SyncMessage};
#[cfg(feature = "gossip_overlay")]
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::admission::AdmissionDecision;
use crate::peer::dial_loop;
use crate::state::{OfflineReason, PeerConnState};
use crate::transport::Shared;

/// How many warm dial loops a node maintains toward other active members when
/// the `gossip_overlay` feature is on (issue #171 / spec §4 D3). The `HyParView`
/// partial view maintained by `iroh-gossip` grows the overlay the rest of the
/// way, so the warm-link count is bounded by a small constant K rather than
/// `O(N)` — the change that lets N grow past the QUIC-connection-count wall the
/// spike-N40 measured at N=40.
///
/// K=3 is the recommended v1 constant: enough for redundancy at the target
/// N=20 (D7), small enough that the warm connection count is bounded regardless
/// of N. Revisit if Phase B's spike-N40 re-run shows connectedness <95%.
pub const GOSSIP_BOOTSTRAP_SEEDS: usize = 3;

/// Per-node cap on simultaneously-live on-demand dial loops toward non-seed
/// active members (issue #171 / spec §4 D3 step 4). When the engine targets a
/// pull/query at a peer that is not a warm seed, `Shared::route` triggers a
/// transient dial that tears itself down after a quiescence timeout. The cap
/// keeps the total per-node connection count at `K + MAX_ON_DEMAND_LINKS`
/// regardless of N.
pub const MAX_ON_DEMAND_LINKS: usize = 8;

/// Number of pull/query frames that may wait for one on-demand connection to
/// come up. Each frame is independently bounded by `MAX_FRAME_BYTES`; keeping
/// this queue small prevents an unreachable non-seed from accumulating
/// unbounded engine output.
#[cfg(feature = "gossip_overlay")]
const ON_DEMAND_PENDING_FRAMES: usize = 16;

/// One running managed dial loop: the task handle and the address it is dialing.
struct DialEntry {
    handle: JoinHandle<()>,
    /// The address the loop is dialing — retained so a future `--peer` hint change
    /// could be detected (spec §4.2 step 4 / D4; a no-op while hints are immutable).
    #[allow(dead_code)]
    addr: EndpointAddr,
}

#[cfg(feature = "gossip_overlay")]
struct OnDemandEntry {
    tx: mpsc::Sender<Outgoing>,
    handle: JoinHandle<()>,
}

/// Room-scoped manager of the outbound dial set (spec §4.1).
///
/// Holds handles to dial the endpoint and mutate the shared per-peer tables; it does
/// **not** own the engine or the `Router`. One [`dial_loop`] runs per desired
/// active-member device, kept reconciled against the live membership snapshot.
pub struct PeerManager {
    /// Per-peer tables + admission source + audit sink.
    shared: Arc<Shared>,
    /// The endpoint the dial loops dial from.
    endpoint: Endpoint,
    /// This node's own device — never dialed.
    self_device: EndpointId,
    /// `device → running dial loop`. Interior mutability so the pump can reconcile
    /// through a shared `&self` while `Node`/`Drop` also touch it.
    dials: Mutex<HashMap<EndpointId, DialEntry>>,
    /// Operator-supplied addressing hints (`--peer`), by device id. Immutable for a
    /// session (spec D4).
    addr_hints: HashMap<EndpointId, EndpointAddr>,
    /// Full admitted-member set. Warm dials use only `desired_seeds`, while
    /// this set authorizes lazy point-to-point pulls to non-seeds.
    #[cfg(feature = "gossip_overlay")]
    active_links: Mutex<BTreeSet<EndpointId>>,
    /// Lazily-created, quiescence-bounded `EVENT_ALPN` links for pull/query
    /// variants targeting active members outside the warm seed set.
    #[cfg(feature = "gossip_overlay")]
    on_demand: Mutex<HashMap<EndpointId, OnDemandEntry>>,
}

impl PeerManager {
    /// Create a manager over the shared transport state. `addr_hints` are the
    /// `--peer` addresses the operator supplied, indexed by device for resolution.
    #[must_use]
    pub fn new(
        shared: Arc<Shared>,
        endpoint: Endpoint,
        self_device: EndpointId,
        addr_hints: Vec<EndpointAddr>,
    ) -> Self {
        let addr_hints = addr_hints.into_iter().map(|a| (a.id, a)).collect();
        Self {
            shared,
            endpoint,
            self_device,
            dials: Mutex::new(HashMap::new()),
            addr_hints,
            #[cfg(feature = "gossip_overlay")]
            active_links: Mutex::new(BTreeSet::new()),
            #[cfg(feature = "gossip_overlay")]
            on_demand: Mutex::new(HashMap::new()),
        }
    }

    /// The desired outbound connection set for `snapshot`: every **active** member's
    /// bound device, minus `self_device`, deduplicated (a member may bind more than
    /// one device across the fold history, but the snapshot's bound device is the
    /// single current one). This is the promoted form of the CLI's `build_dial_set`
    /// device selection, so there is one implementation (spec §4.2 step 1 / step 1
    /// of §11).
    #[must_use]
    pub fn desired_devices(
        snapshot: &MembershipSnapshot,
        self_device: EndpointId,
    ) -> BTreeSet<EndpointId> {
        let mut out = BTreeSet::new();
        for m in snapshot.active_members() {
            let Some(dev) = m.device else { continue };
            let Ok(id) = EndpointId::from_bytes(dev.as_bytes()) else {
                continue;
            };
            if id != self_device {
                out.insert(id);
            }
        }
        out
    }

    /// The gossip-bootstrap seed set: a K-bounded deterministic subset of
    /// [`desired_devices`](Self::desired_devices) (issue #171 / spec §4 D3).
    ///
    /// Selection (D3 step 2): the K lowest-bytewise `EndpointId`s among active
    /// members (excluding self), plus the room admin's device if it is Active
    /// and not self and not already in the seed set. Determinism is
    /// load-bearing: every node must compute the same seed set from the same
    /// snapshot so the seeds are mutually reachable (a node's seed set is who
    /// it dials warm; mismatches would leave some seeds unreachable).
    ///
    /// K is [`GOSSIP_BOOTSTRAP_SEEDS`] (3 for v1). At small N (N-1 ≤ K) the
    /// seed set equals the full desired set, so the seed selector is a no-op
    /// for no-gossip/default builds where `MAX_ACTIVE_MEMBERS = 5`; when
    /// `gossip_overlay` enables the larger cap, the selector bounds warm links
    /// once N grows past K+1.
    ///
    /// The returned set is the **warm dial target** — `reconcile` starts/stops
    /// loops only for these devices when the overlay is on. Pull/query
    /// variants to non-seed peers ride on-demand dials bounded by
    /// [`MAX_ON_DEMAND_LINKS`] (D3 step 4).
    #[must_use]
    pub fn desired_seeds(
        snapshot: &MembershipSnapshot,
        self_device: EndpointId,
    ) -> BTreeSet<EndpointId> {
        // `desired_devices` returns a `BTreeSet<EndpointId>`; iteration is
        // already in bytewise order, so the first K are the K lowest-bytewise.
        let desired = Self::desired_devices(snapshot, self_device);
        let mut seeds: BTreeSet<EndpointId> =
            desired.into_iter().take(GOSSIP_BOOTSTRAP_SEEDS).collect();

        // The room admin (if Active and not self) is always a seed: it is the
        // stable rendezvous point for join bootstrap and the canonical source
        // of the admin chain. The admin is identified by the snapshot's
        // genesis `admin` identity, so a member is the admin iff its identity
        // equals `snapshot.admin()`. `active_members` already includes the
        // admin when Active, so this is a guarantee, not an addition, when
        // the admin is among the K lowest-bytewise; otherwise it widens the
        // set past K by exactly one (acceptable: K+1 warm links, still O(1)).
        let admin_identity = snapshot.admin();
        for m in snapshot.active_members() {
            if Some(&m.identity) != admin_identity {
                continue;
            }
            let Some(dev) = m.device else { continue };
            let Ok(id) = EndpointId::from_bytes(dev.as_bytes()) else {
                continue;
            };
            if id != self_device {
                seeds.insert(id);
            }
        }
        seeds
    }

    /// Resolve a device to a dialable [`EndpointAddr`]: an explicit `--peer` hint if
    /// one matches (deterministic loopback/LAN), else a bare `EndpointId` (iroh
    /// discovery resolves it in real-network mode) — identical to `build_dial_set`.
    fn resolve_addr(&self, device: EndpointId) -> EndpointAddr {
        self.addr_hints
            .get(&device)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(device))
    }

    /// Reconcile the running dial-loop set against `snapshot` (spec §4.2).
    ///
    /// **Idempotent**: called with an unchanged snapshot it is a no-op — no loop
    /// churn, no spurious `ConnEvent`s (spec R4) — because it diffs the desired set
    /// against the running set and only acts on genuine deltas.
    ///
    /// * **Start** a loop (and set `Connecting`, first-sight) for each newly-desired
    ///   device with no running loop.
    /// * **Stop** — abort the loop, close any live link, unregister, mark
    ///   `Offline{Deauthorized}`, and audit `peer.deauthorized` — for each running
    ///   loop whose device is no longer desired (removed / left the room).
    pub fn reconcile(&self, snapshot: &MembershipSnapshot) {
        self.reconcile_inner(snapshot, false);
    }

    /// Production reconcile after the room driver has refreshed the live
    /// admission cell. In addition to membership removal, this tears down
    /// fail-closed subjects that remain nominally Active in the fold.
    pub(crate) fn reconcile_admitted(&self, snapshot: &MembershipSnapshot) {
        self.reconcile_inner(snapshot, true);
    }

    fn reconcile_inner(&self, snapshot: &MembershipSnapshot, enforce_admission: bool) {
        // Membership is necessary but not sufficient: fail-closed subjects
        // remain nominally Active in the fold and must still lose every live
        // transport. The admission cell is refreshed immediately before this
        // production entry point is called, so filtering through it ties direct
        // links to the same revocation lifecycle as the accept gates. The public
        // `reconcile` keeps its historical pure membership-set contract for
        // focused manager users/tests that deliberately provide no admission.
        let membership_links = Self::desired_devices(snapshot, self.self_device);
        let admitted_links: BTreeSet<EndpointId> = if enforce_admission {
            membership_links
                .into_iter()
                .filter(|device| {
                    matches!(
                        self.shared.admission.authorize(*device),
                        AdmissionDecision::Admit { .. }
                    )
                })
                .collect()
        } else {
            membership_links
        };

        // The desired warm dial set: every active member (full mesh) when the
        // gossip overlay is off — the path the v1 spike measured at N≤5. When
        // the overlay is on, only the K-bounded seed subset is dialed warm;
        // the HyParView partial view maintained by iroh-gossip grows the
        // overlay the rest of the way, and pull/query frames to non-seed
        // peers ride on-demand dials bounded by `MAX_ON_DEMAND_LINKS`
        // (issue #171 / spec §4 D3 step 3). At N≤5 and K=3 the seed set equals
        // the full desired set whenever N-1 ≤ K, so the selector is a no-op
        // for default no-gossip builds that keep `MAX_ACTIVE_MEMBERS = 5`.
        #[cfg(feature = "gossip_overlay")]
        let desired: BTreeSet<EndpointId> = Self::desired_seeds(snapshot, self.self_device)
            .intersection(&admitted_links)
            .copied()
            .collect();
        #[cfg(not(feature = "gossip_overlay"))]
        let desired = admitted_links.clone();

        #[cfg(feature = "gossip_overlay")]
        {
            self.active_links
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone_from(&admitted_links);

            // A removed/fail-closed peer must lose any transient link too.
            let mut on_demand = self
                .on_demand
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let stale: Vec<_> = on_demand
                .keys()
                .filter(|device| !admitted_links.contains(*device))
                .copied()
                .collect();
            for device in stale {
                if let Some(entry) = on_demand.remove(&device) {
                    entry.handle.abort();
                }
                self.shared.close_connection(device);
            }
        }

        let mut dials = self
            .dials
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Stop: running loops whose device left the desired set.
        let to_stop: Vec<EndpointId> = dials
            .keys()
            .filter(|d| !desired.contains(*d))
            .copied()
            .collect();
        for device in to_stop {
            if let Some(entry) = dials.remove(&device) {
                entry.handle.abort();
                // Tear down any in-flight link so we stop serving a now-removed peer,
                // then mark the entry terminal so the CLI can show "stopped dialing".
                // Invalidate the connection generation *before* closing the link
                // (issue #126 follow-up): an inbound accept task, woken by the close,
                // would otherwise run its own `teardown_if_current` and overwrite this
                // terminal `Deauthorized` with a generic `LinkDropped`. Bumping the
                // generation first makes that late teardown a no-op, so our forced
                // teardown below is the one that sticks.
                self.shared.invalidate_link(device);
                self.shared.close_connection(device);
                self.shared.unregister(device);
                if admitted_links.contains(&device) {
                    // Still Active but no longer selected as a warm seed.
                    self.shared
                        .table
                        .set_offline(device, OfflineReason::LinkDropped, None);
                } else {
                    self.shared
                        .table
                        .set_offline(device, OfflineReason::Deauthorized, None);
                    self.shared.audit.deauthorized(device);
                }
            }
        }

        // Start: newly-desired devices with no running loop.
        for device in &desired {
            if dials.contains_key(device) {
                continue;
            }
            let addr = self.resolve_addr(*device);
            // Set Connecting (first-sight) so the operator sees the attempt even
            // before the loop's own first `Connecting` set runs.
            self.shared
                .table
                .set(*device, PeerConnState::Connecting, None);
            let handle = tokio::spawn(dial_loop(
                self.shared.clone(),
                self.endpoint.clone(),
                addr.clone(),
            ));
            dials.insert(*device, DialEntry { handle, addr });
        }
    }

    /// The number of dial loops currently running (observability / tests).
    #[must_use]
    pub fn dial_count(&self) -> usize {
        self.dials
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Queue a targeted pull on a bounded, lazy `EVENT_ALPN` link when the peer is
    /// Active but not currently connected as a warm seed. Called synchronously
    /// by `Shared::route`; dialing and delivery happen in the spawned task.
    #[cfg(feature = "gossip_overlay")]
    pub(crate) fn route_on_demand(&self, out: Outgoing) {
        if !matches!(
            &out.msg,
            SyncMessage::WantMembership { .. }
                | SyncMessage::WantRecentChat { .. }
                | SyncMessage::WantEvents { .. }
        ) {
            return;
        }
        let Ok(device) = EndpointId::from_bytes(out.peer.as_bytes()) else {
            return;
        };
        if !self
            .active_links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&device)
        {
            return;
        }

        let mut entries = self
            .on_demand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, entry| !entry.handle.is_finished());
        if let Some(entry) = entries.get(&device) {
            if entry.tx.try_send(out).is_err() {
                self.shared
                    .audit
                    .transport_queue_saturated(device, "on_demand");
            }
            return;
        }
        if entries.len() >= MAX_ON_DEMAND_LINKS {
            self.shared
                .audit
                .transport_queue_saturated(device, "on_demand");
            return;
        }

        let (tx, rx) = mpsc::channel(ON_DEMAND_PENDING_FRAMES);
        if tx.try_send(out).is_err() {
            return;
        }
        let handle = tokio::spawn(crate::peer::on_demand_dial(
            self.shared.clone(),
            self.endpoint.clone(),
            self.resolve_addr(device),
            rx,
        ));
        entries.insert(device, OnDemandEntry { tx, handle });
    }

    /// Number of live/connecting transient links (tests and diagnostics).
    #[cfg(feature = "gossip_overlay")]
    #[cfg(test)]
    #[must_use]
    pub(crate) fn on_demand_count(&self) -> usize {
        self.on_demand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|entry| !entry.handle.is_finished())
            .count()
    }

    /// Abort every running dial loop (idempotent; also runs on `Drop`).
    pub fn shutdown(&self) {
        let mut dials = self
            .dials
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, entry) in dials.drain() {
            entry.handle.abort();
        }
        #[cfg(feature = "gossip_overlay")]
        for (_, entry) in self
            .on_demand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
        {
            entry.handle.abort();
        }
    }
}

impl Drop for PeerManager {
    fn drop(&mut self) {
        // Abort the loops so they do not outlive the manager (graceful shutdown
        // beyond Drop is a non-goal, spec N6).
        if let Ok(mut dials) = self.dials.lock() {
            for (_, entry) in dials.drain() {
                entry.handle.abort();
            }
        }
        #[cfg(feature = "gossip_overlay")]
        if let Ok(mut on_demand) = self.on_demand.lock() {
            for (_, entry) in on_demand.drain() {
                entry.handle.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PeerManager;
    use iroh::{EndpointId, SecretKey};
    use iroh_rooms_core::event::binding::DeviceBinding;
    use iroh_rooms_core::event::content::{
        capability_hash, Content, EventType, MemberInvited, MemberJoined, MemberRemoved,
        RoomCreated,
    };
    use iroh_rooms_core::event::ids::{EventId, RoomId};
    use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
    use iroh_rooms_core::event::signed::{self, SignedEvent};
    use iroh_rooms_core::event::validate::{
        validate_wire_bytes, ValidatedEvent, ValidationContext,
    };
    use iroh_rooms_core::event::wire::WireEvent;
    use iroh_rooms_core::membership::RoomMembership;
    #[cfg(feature = "gossip_overlay")]
    use iroh_rooms_core::sync::{Outgoing, PeerId, SyncMessage};
    #[cfg(feature = "gossip_overlay")]
    use std::sync::Arc;

    #[cfg(feature = "gossip_overlay")]
    use crate::admission::{Admission, AllowlistAdmission};
    #[cfg(feature = "gossip_overlay")]
    use crate::audit::TracingAudit;
    #[cfg(feature = "gossip_overlay")]
    use crate::transport::{NetConfig, NetTransport};

    // Deterministic fixtures shared across the tests below.
    const NONCE: [u8; 16] = [0xcc; 16];
    const T0: u64 = 1_750_000_000_000;

    /// A test actor: one identity key + one device key. Same seed derivation as
    /// `demo::Participant` so `endpoint_id == device_id` byte-for-byte (spec A2).
    struct Actor {
        id_sk: SigningKey,
        dev_sk: SigningKey,
    }

    impl Actor {
        fn new(seed: u8) -> Self {
            Self {
                id_sk: SigningKey::from_seed(&[seed; 32]),
                dev_sk: SigningKey::from_seed(&[seed.wrapping_add(0x80); 32]),
            }
        }
        fn identity(&self) -> IdentityKey {
            self.id_sk.identity_key()
        }
        fn device(&self) -> DeviceKey {
            self.dev_sk.device_key()
        }
        /// The iroh `EndpointId` — equal byte-for-byte to the device key (spec A2).
        fn endpoint_id(&self) -> EndpointId {
            SecretKey::from_bytes(&self.dev_sk.to_seed()).public()
        }
    }

    /// Seal a `SignedEvent` and run it through the stateless validator.
    fn validate(ev: &SignedEvent, dev_sk: &SigningKey, room_id: RoomId) -> ValidatedEvent {
        let csb = ev.to_csb();
        let sig = signed::sign_csb(&csb, dev_sk);
        let bytes = WireEvent::seal(csb, sig).to_bytes();
        validate_wire_bytes(&bytes, &ValidationContext::for_room(room_id))
            .expect("test event must be stateless-valid")
    }

    /// Build a `room.created` event (makes `admin` Active).
    fn mk_genesis(admin: &Actor) -> (RoomId, ValidatedEvent) {
        let room_id = signed::derive_room_id(&admin.identity(), &NONCE, T0);
        let binding = DeviceBinding::create(&room_id, &admin.id_sk, admin.device());
        let ev = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: admin.identity(),
            device_id: admin.device(),
            event_type: EventType::RoomCreated,
            created_at: T0,
            prev_events: vec![],
            content: Content::RoomCreated(RoomCreated {
                room_name: "mgr-test".to_owned(),
                room_nonce: NONCE,
                admins: vec![admin.identity()],
                device_binding: binding,
            }),
        };
        (room_id, validate(&ev, &admin.dev_sk, room_id))
    }

    /// Build a `member.invited` event. `n` seeds the `invite_id` / secret so
    /// multiple members can be invited without id collisions.
    fn mk_invite(
        admin: &Actor,
        room_id: RoomId,
        prev: &[EventId],
        invitee: IdentityKey,
        n: u8,
        ts: u64,
    ) -> ValidatedEvent {
        let invite_id = [n; 16];
        let secret = [n.wrapping_add(1); 16];
        let cap_hash = capability_hash(&room_id, &invite_id, &secret);
        let ev = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: admin.identity(),
            device_id: admin.device(),
            event_type: EventType::MemberInvited,
            created_at: ts,
            prev_events: prev.to_vec(),
            content: Content::MemberInvited(MemberInvited {
                invite_id,
                capability_hash: cap_hash,
                role: "member".to_owned(),
                invitee_key: invitee,
                expires_at: None,
                invitee_hint: None,
            }),
        };
        validate(&ev, &admin.dev_sk, room_id)
    }

    /// Build a `member.joined` event. `n` must match the corresponding invite.
    fn mk_join(
        member: &Actor,
        room_id: RoomId,
        prev: &[EventId],
        n: u8,
        ts: u64,
    ) -> ValidatedEvent {
        let invite_id = [n; 16];
        let secret = [n.wrapping_add(1); 16];
        let binding = DeviceBinding::create(&room_id, &member.id_sk, member.device());
        let ev = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: member.identity(),
            device_id: member.device(),
            event_type: EventType::MemberJoined,
            created_at: ts,
            prev_events: prev.to_vec(),
            content: Content::MemberJoined(MemberJoined {
                via_invite_id: invite_id,
                capability_secret: secret,
                role: "member".to_owned(),
                device_binding: binding,
                display_name: None,
            }),
        };
        validate(&ev, &member.dev_sk, room_id)
    }

    /// Build a `member.removed` event (admin removes `target`).
    fn mk_remove(
        admin: &Actor,
        room_id: RoomId,
        prev: &[EventId],
        target: IdentityKey,
        ts: u64,
    ) -> ValidatedEvent {
        let ev = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: admin.identity(),
            device_id: admin.device(),
            event_type: EventType::MemberRemoved,
            created_at: ts,
            prev_events: prev.to_vec(),
            content: Content::MemberRemoved(MemberRemoved {
                member_id: target,
                removed_by: admin.identity(),
                reason: None,
                device_binding: None,
            }),
        };
        validate(&ev, &admin.dev_sk, room_id)
    }

    // -----------------------------------------------------------------------
    // desired_devices: pure function — no endpoint needed
    // -----------------------------------------------------------------------

    #[test]
    fn desired_devices_empty_fold_is_empty() {
        let room_id = RoomId::from_bytes([0u8; 32]);
        let snapshot = RoomMembership::new(room_id).snapshot();
        let self_dev = SecretKey::from_bytes(&[1u8; 32]).public();
        assert!(
            PeerManager::desired_devices(&snapshot, self_dev).is_empty(),
            "empty fold must yield an empty desired set"
        );
    }

    #[test]
    fn desired_devices_excludes_self() {
        // Only the admin is Active after a genesis fold. If admin == self, the
        // set must be empty (spec §4.2 step 1 "minus self_device").
        let admin = Actor::new(1);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let snapshot = RoomMembership::from_events(room_id, [genesis_ev]).snapshot();

        assert!(
            PeerManager::desired_devices(&snapshot, admin.endpoint_id()).is_empty(),
            "self device must be excluded from the desired set"
        );
    }

    #[test]
    fn desired_devices_includes_active_admin_when_not_self() {
        // When self ≠ admin, the Active admin's device IS desired.
        let admin = Actor::new(1);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let snapshot = RoomMembership::from_events(room_id, [genesis_ev]).snapshot();

        let outsider: EndpointId = SecretKey::from_bytes(&[99u8; 32]).public();
        let result = PeerManager::desired_devices(&snapshot, outsider);
        assert_eq!(result.len(), 1);
        assert!(
            result.contains(&admin.endpoint_id()),
            "active admin's endpoint must appear in the desired set"
        );
    }

    #[test]
    fn desired_devices_includes_active_member_excludes_self() {
        // Two-actor room (admin + member, both Active after genesis+invite+join).
        // With self = admin, only member must be desired; vice versa.
        let admin = Actor::new(1);
        let member = Actor::new(2);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
        let iid = invite_ev.event_id;
        let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);

        let snapshot =
            RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev]).snapshot();

        // Self = admin → only member desired.
        let r = PeerManager::desired_devices(&snapshot, admin.endpoint_id());
        assert_eq!(r.len(), 1);
        assert!(r.contains(&member.endpoint_id()));

        // Self = member → only admin desired.
        let r2 = PeerManager::desired_devices(&snapshot, member.endpoint_id());
        assert_eq!(r2.len(), 1);
        assert!(r2.contains(&admin.endpoint_id()));
    }

    #[test]
    fn desired_devices_skips_invited_only_member() {
        // An Invited member who has not joined yet has no device binding in the
        // fold (`device == None` in Member); `desired_devices` must skip it.
        let admin = Actor::new(1);
        let invitee = Actor::new(2);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite_ev = mk_invite(&admin, room_id, &[gid], invitee.identity(), 1, T0 + 1);

        // No join event → invitee is Invited but has no device.
        let snapshot = RoomMembership::from_events(room_id, [genesis_ev, invite_ev]).snapshot();

        let outsider: EndpointId = SecretKey::from_bytes(&[99u8; 32]).public();
        let result = PeerManager::desired_devices(&snapshot, outsider);
        // Only the admin (Active + device) must appear.
        assert_eq!(result.len(), 1, "invited-only member must be skipped");
        assert!(result.contains(&admin.endpoint_id()));
        assert!(
            !result.contains(&invitee.endpoint_id()),
            "invited-but-not-joined device must not appear"
        );
    }

    #[test]
    fn desired_devices_three_active_members_self_is_admin() {
        // Three-actor room: admin (self) + member1 + member2. Both non-self active
        // members must appear in the desired set; admin (self) must be excluded.
        let admin = Actor::new(5);
        let m1 = Actor::new(6);
        let m2 = Actor::new(7);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
        let i1id = invite1.event_id;
        let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
        let j1id = join1.event_id;
        let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
        let i2id = invite2.event_id;
        let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);

        let snapshot =
            RoomMembership::from_events(room_id, [genesis_ev, invite1, join1, invite2, join2])
                .snapshot();

        let result = PeerManager::desired_devices(&snapshot, admin.endpoint_id());
        assert_eq!(
            result.len(),
            2,
            "three-actor room with self=admin: exactly two devices desired"
        );
        assert!(
            result.contains(&m1.endpoint_id()),
            "m1 must be in the desired set"
        );
        assert!(
            result.contains(&m2.endpoint_id()),
            "m2 must be in the desired set"
        );
        assert!(
            !result.contains(&admin.endpoint_id()),
            "admin (self) must be excluded"
        );
    }

    #[test]
    fn desired_devices_excludes_removed_member() {
        // A removed member is `Status::Removed`; `active_members()` excludes them,
        // so `desired_devices` must also exclude them (spec §4.2 "Active members").
        let admin = Actor::new(1);
        let member = Actor::new(2);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite_ev = mk_invite(&admin, room_id, &[gid], member.identity(), 1, T0 + 1);
        let iid = invite_ev.event_id;
        let join_ev = mk_join(&member, room_id, &[iid], 1, T0 + 2);
        let jid = join_ev.event_id;
        let remove_ev = mk_remove(&admin, room_id, &[jid], member.identity(), T0 + 3);

        let snapshot =
            RoomMembership::from_events(room_id, [genesis_ev, invite_ev, join_ev, remove_ev])
                .snapshot();

        let outsider: EndpointId = SecretKey::from_bytes(&[99u8; 32]).public();
        let result = PeerManager::desired_devices(&snapshot, outsider);
        assert_eq!(result.len(), 1, "removed member must be excluded");
        assert!(result.contains(&admin.endpoint_id()));
        assert!(
            !result.contains(&member.endpoint_id()),
            "removed member's device must not be in the desired set"
        );
    }

    // -----------------------------------------------------------------------
    // desired_seeds: the K-bounded gossip-bootstrap subset (issue #171 / §4 D3)
    // -----------------------------------------------------------------------
    //
    // `desired_seeds` selects the K lowest-bytewise active devices + the room
    // admin (if Active and not self). Determinism is load-bearing: every node
    // must compute the same seed set from the same snapshot so the seeds are
    // mutually reachable. At N-1 ≤ K the seed set equals the full desired set.

    #[test]
    fn desired_seeds_empty_fold_is_empty() {
        let room_id = RoomId::from_bytes([0u8; 32]);
        let snapshot = RoomMembership::new(room_id).snapshot();
        let self_dev = SecretKey::from_bytes(&[1u8; 32]).public();
        assert!(
            PeerManager::desired_seeds(&snapshot, self_dev).is_empty(),
            "empty fold must yield an empty seed set"
        );
    }

    #[test]
    fn desired_seeds_excludes_self() {
        // Only the admin is Active after a genesis fold. If admin == self, the
        // seed set must be empty.
        let admin = Actor::new(1);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let snapshot = RoomMembership::from_events(room_id, [genesis_ev]).snapshot();

        assert!(
            PeerManager::desired_seeds(&snapshot, admin.endpoint_id()).is_empty(),
            "self device must be excluded from the seed set"
        );
    }

    #[test]
    fn desired_seeds_at_small_n_equals_full_desired_set() {
        // At N=3 (admin + m1 + m2) and K=3, every non-self active member is a
        // seed — the no-op at the current cap. Pin this so the seed selector
        // never accidentally drops a peer at N≤5 (where the spike measured the
        // mesh path).
        let admin = Actor::new(5);
        let m1 = Actor::new(6);
        let m2 = Actor::new(7);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
        let i1id = invite1.event_id;
        let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
        let j1id = join1.event_id;
        let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
        let i2id = invite2.event_id;
        let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);

        let snapshot =
            RoomMembership::from_events(room_id, [genesis_ev, invite1, join1, invite2, join2])
                .snapshot();

        let seeds = PeerManager::desired_seeds(&snapshot, admin.endpoint_id());
        let full = PeerManager::desired_devices(&snapshot, admin.endpoint_id());
        assert_eq!(
            seeds, full,
            "at N=3 with K=3, every non-self active member must be a seed"
        );
    }

    #[test]
    fn desired_seeds_is_bounded_by_k_plus_admin_at_large_n() {
        // At N=12 (admin + 11 members) and K=3, the seed set is at most K+1
        // (the K lowest-bytewise members + the admin, if not already among
        // them). This is the load-bearing property: the warm-link count is
        // bounded regardless of N (issue #171 / spec §4 D3).
        let admin = Actor::new(1);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let mut prev = genesis_ev.event_id;
        let mut events = vec![genesis_ev];
        for n in 2u8..=12 {
            let m = Actor::new(n);
            let invite = mk_invite(&admin, room_id, &[prev], m.identity(), n, T0 + u64::from(n));
            let iid = invite.event_id;
            let join = mk_join(&m, room_id, &[iid], n, T0 + u64::from(n) + 100);
            prev = join.event_id;
            events.push(invite);
            events.push(join);
        }
        let snapshot = RoomMembership::from_events(room_id, events).snapshot();

        // Self is an outsider so the admin counts toward the seed set.
        let outsider: EndpointId = SecretKey::from_bytes(&[99u8; 32]).public();
        let seeds = PeerManager::desired_seeds(&snapshot, outsider);
        let k = super::GOSSIP_BOOTSTRAP_SEEDS;
        assert!(
            seeds.len() <= k + 1,
            "seed set must be bounded by K+1 (K lowest-bytewise + admin); got {} (K={k})",
            seeds.len()
        );
        assert!(
            seeds.contains(&admin.endpoint_id()),
            "the active admin must always be a seed"
        );
    }

    #[test]
    fn desired_seeds_is_deterministic_across_calls() {
        // Every node must compute the same seed set from the same snapshot, so
        // the selection must be a pure function of (snapshot, self_device).
        let admin = Actor::new(5);
        let m1 = Actor::new(6);
        let m2 = Actor::new(7);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
        let i1id = invite1.event_id;
        let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
        let j1id = join1.event_id;
        let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
        let i2id = invite2.event_id;
        let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);

        let snapshot =
            RoomMembership::from_events(room_id, [genesis_ev, invite1, join1, invite2, join2])
                .snapshot();

        let outsider: EndpointId = SecretKey::from_bytes(&[99u8; 32]).public();
        assert_eq!(
            PeerManager::desired_seeds(&snapshot, outsider),
            PeerManager::desired_seeds(&snapshot, outsider),
            "seed selection must be deterministic"
        );
    }

    #[test]
    fn desired_seeds_is_a_subset_of_desired_devices() {
        // The seed selector narrows the desired set; it must never invent a
        // device that is not an active member's bound device (minus self). This
        // is the load-bearing invariant for "only admitted peers are dialed
        // warm" (issue #171 / spec §4 D3): a seed outside `desired_devices`
        // would mean dialing a device that admission would reject, or — worse
        // — a device the snapshot does not know at all. Pinned at large N so
        // both the K-take and the admin-injection paths are exercised.
        let admin = Actor::new(1);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let mut prev = genesis_ev.event_id;
        let mut events = vec![genesis_ev];
        for n in 2u8..=9 {
            let m = Actor::new(n);
            let invite = mk_invite(&admin, room_id, &[prev], m.identity(), n, T0 + u64::from(n));
            let iid = invite.event_id;
            let join = mk_join(&m, room_id, &[iid], n, T0 + u64::from(n) + 100);
            prev = join.event_id;
            events.push(invite);
            events.push(join);
        }
        let snapshot = RoomMembership::from_events(room_id, events).snapshot();

        // Self is an outsider so admin + members are all candidates.
        let outsider: EndpointId = SecretKey::from_bytes(&[99u8; 32]).public();
        let seeds = PeerManager::desired_seeds(&snapshot, outsider);
        let desired = PeerManager::desired_devices(&snapshot, outsider);
        assert!(
            seeds.is_subset(&desired),
            "seed set ({seeds:?}) must be a subset of desired devices ({desired:?}); \
             a warm-dial target outside the active set would bypass admission"
        );

        // And self must never appear in either set.
        assert!(
            !seeds.contains(&outsider),
            "self must never be a warm-dial seed target"
        );
    }

    #[test]
    fn desired_seeds_excludes_admin_when_admin_is_self_even_with_members() {
        // The `desired_seeds_excludes_self` test only covers the genesis case
        // (admin alone). Here self == admin AND there are active members, so
        // the seed set is non-empty but must still exclude self. Catches a
        // regression where the admin-injection path re-adds self when self is
        // the admin.
        let admin = Actor::new(3);
        let m1 = Actor::new(4);
        let m2 = Actor::new(5);
        let (room_id, genesis_ev) = mk_genesis(&admin);
        let gid = genesis_ev.event_id;
        let invite1 = mk_invite(&admin, room_id, &[gid], m1.identity(), 1, T0 + 1);
        let i1id = invite1.event_id;
        let join1 = mk_join(&m1, room_id, &[i1id], 1, T0 + 2);
        let j1id = join1.event_id;
        let invite2 = mk_invite(&admin, room_id, &[j1id], m2.identity(), 2, T0 + 3);
        let i2id = invite2.event_id;
        let join2 = mk_join(&m2, room_id, &[i2id], 2, T0 + 4);

        let snapshot =
            RoomMembership::from_events(room_id, [genesis_ev, invite1, join1, invite2, join2])
                .snapshot();

        let seeds = PeerManager::desired_seeds(&snapshot, admin.endpoint_id());
        assert!(
            !seeds.contains(&admin.endpoint_id()),
            "admin's own device must be excluded from its seed set even with members present"
        );
        assert!(
            !seeds.is_empty(),
            "with two active non-self members, the seed set must be non-empty"
        );
    }

    #[test]
    fn gossip_bootstrap_seeds_constant_is_the_v1_value() {
        // K=3 is the recommended v1 constant (spec §4 D3 step 1). Pin it so a
        // future change is caught: raising K widens the warm-link count and
        // must be re-justified by Phase B's spike-N40 re-run.
        assert_eq!(super::GOSSIP_BOOTSTRAP_SEEDS, 3);
    }

    #[test]
    fn max_on_demand_links_constant_is_the_v1_value() {
        // The per-node transient-dial cap (D3 step 4). K + MAX_ON_DEMAND_LINKS
        // is the total per-node connection count bound.
        assert_eq!(super::MAX_ON_DEMAND_LINKS, 8);
    }

    /// Review regression: the fourth non-self member at the current five-member
    /// cap is outside K=3. A targeted pull to that member must create a bounded
    /// transient link and deliver the frame instead of being silently dropped.
    #[cfg(feature = "gossip_overlay")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)] // full queued-dial boundary setup and teardown
    async fn non_seed_pull_dials_on_demand_and_delivers_queued_frame() {
        let admin = Actor::new(0x21);
        let members = [
            Actor::new(0x22),
            Actor::new(0x23),
            Actor::new(0x24),
            Actor::new(0x25),
        ];
        let (room_id, genesis) = mk_genesis(&admin);
        let mut events = vec![genesis];
        let mut prev = events[0].event_id;
        for (index, member) in members.iter().enumerate() {
            let n = u8::try_from(index + 1).expect("small fixture");
            let invite = mk_invite(
                &admin,
                room_id,
                &[prev],
                member.identity(),
                n,
                T0 + u64::from(n) * 2 - 1,
            );
            let join = mk_join(
                member,
                room_id,
                &[invite.event_id],
                n,
                T0 + u64::from(n) * 2,
            );
            prev = join.event_id;
            events.push(invite);
            events.push(join);
        }
        let snapshot = RoomMembership::from_events(room_id, events).snapshot();
        let desired = PeerManager::desired_devices(&snapshot, admin.endpoint_id());
        let seeds = PeerManager::desired_seeds(&snapshot, admin.endpoint_id());
        let omitted = *desired
            .difference(&seeds)
            .next()
            .expect("five-member room has one non-seed target at K=3");
        let target_actor = members
            .iter()
            .find(|member| member.endpoint_id() == omitted)
            .expect("omitted endpoint belongs to a fixture member");

        let source_gate = members
            .iter()
            .fold(AllowlistAdmission::new(), |gate, member| {
                gate.bind_device(member.endpoint_id(), member.identity())
                    .set_active(member.identity())
            });
        let target_gate = AllowlistAdmission::new()
            .bind_device(admin.endpoint_id(), admin.identity())
            .set_active(admin.identity());
        let mut target = NetTransport::bind(
            SecretKey::from_bytes(&target_actor.dev_sk.to_seed()),
            Arc::new(target_gate) as Arc<dyn Admission>,
            Arc::new(TracingAudit),
            NetConfig::default(),
            None,
            None,
        )
        .await
        .expect("bind target transport");
        let target_addr = target.endpoint_addr().expect("target loopback address");
        let mut target_inbound = target.take_inbound().expect("target inbound receiver");

        let source = NetTransport::bind(
            SecretKey::from_bytes(&admin.dev_sk.to_seed()),
            Arc::new(source_gate) as Arc<dyn Admission>,
            Arc::new(TracingAudit),
            NetConfig::default(),
            None,
            None,
        )
        .await
        .expect("bind source transport");
        let shared = source.shared();
        let manager = Arc::new(PeerManager::new(
            shared.clone(),
            source.endpoint(),
            source.id(),
            vec![target_addr],
        ));
        shared.set_peer_manager(Arc::downgrade(&manager));
        manager.reconcile(&snapshot);

        let out = Outgoing {
            peer: PeerId::from_bytes(*omitted.as_bytes()),
            msg: SyncMessage::WantMembership {
                room_id,
                have: Vec::new(),
            },
        };
        let expected = out.msg.encode();
        shared.route(&out);
        let delivered =
            tokio::time::timeout(std::time::Duration::from_secs(5), target_inbound.recv())
                .await
                .expect("on-demand dial must deliver within budget")
                .expect("target inbound remains open");
        assert_eq!(delivered.bytes, expected);
        assert_eq!(manager.on_demand_count(), 1);

        manager.shutdown();
        source.shutdown().await.expect("source shutdown");
        target.shutdown().await.expect("target shutdown");
    }
}
