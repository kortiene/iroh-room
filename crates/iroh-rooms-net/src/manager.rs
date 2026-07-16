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
use tokio::task::JoinHandle;

use crate::peer::dial_loop;
use crate::state::{OfflineReason, PeerConnState};
use crate::transport::Shared;

/// One running managed dial loop: the task handle and the address it is dialing.
struct DialEntry {
    handle: JoinHandle<()>,
    /// The address the loop is dialing — retained so a future `--peer` hint change
    /// could be detected (spec §4.2 step 4 / D4; a no-op while hints are immutable).
    #[allow(dead_code)]
    addr: EndpointAddr,
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
        let desired = Self::desired_devices(snapshot, self.self_device);

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
                self.shared
                    .table
                    .set_offline(device, OfflineReason::Deauthorized, None);
                self.shared.audit.deauthorized(device);
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

    /// Abort every running dial loop (idempotent; also runs on `Drop`).
    pub fn shutdown(&self) {
        let mut dials = self
            .dials
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, entry) in dials.drain() {
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
}
