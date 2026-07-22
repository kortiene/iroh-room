//! End-to-end coverage for issue #144 — the approach-to-ceiling warning emitted
//! by the live `RoomReconciler` when the active-member count crosses the soft
//! threshold (`ACTIVE_MEMBER_WARNING_THRESHOLD = MAX_ACTIVE_MEMBERS - 1 = 4`).
//!
//! These tests cross the **engine-fold → pump-task → `RoomReconciler` →
//! `AuditSink`** boundary — the wiring that turns a live `3 → 4` active-member
//! fold transition into exactly one `room.active_members.near_cap` callback.
//! The pure `active_member_warning_crossed` helper, the audit-sink formatters,
//! and the `active_capacity_line` CLI formatter each have their own unit tests;
//! only an end-to-end run through a real `Node` can prove the pump actually
//! drives the reconciler against each new snapshot and that the one-shot-per-
//! crossing contract (issue acceptance #1) holds against repeated ticks /
//! forced reconciles.
//!
//! Acceptance coverage:
//! * **AC1** — `managed_room_warns_once_when_active_count_crosses_threshold`:
//!   a room growing `3 → 4` active members emits exactly one
//!   `active_member_threshold_reached` callback with `(active=4, max=5,
//!   remaining=1)`; subsequent ticks and forced reconciles do not re-emit
//!   (no per-tick spam).
//! * **OQ-1** — `managed_room_does_not_warn_on_startup_already_at_threshold`:
//!   a node whose first observed snapshot is already at the threshold emits no
//!   initial warning (spec §4 D3 / OQ-1 recommended default — `room members
//!   --status` is the current-state surface, not a startup alarm).
//!
//! All tests use `NetMode::Loopback` (no discovery, no relay, deterministic CI)
//! and need no second node: the warning is a local observer decision over the
//! engine's own fold, independent of whether any peer link is up.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use iroh::{EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::{ACTIVE_MEMBER_WARNING_THRESHOLD, MAX_ACTIVE_MEMBERS};
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    AdmissionView, AuditSink, NetConfig, NetMode, Node, RejectCause, SnapshotAdmission,
    DEFAULT_TICK,
};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Deterministic room nonce — distinct from every other test file.
const NONCE: [u8; 16] = [0x44; 16];
/// Base timestamp (advisory wall-clock anchor).
const T0: u64 = 1_777_000_000_000;

// ---------------------------------------------------------------------------
// Test actor — deterministic key pair seeded from one byte (spec A2).
// ---------------------------------------------------------------------------

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

    fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.dev_sk.to_seed())
    }
}

// ---------------------------------------------------------------------------
// Event builders
// ---------------------------------------------------------------------------

/// Seal a `SignedEvent` into verbatim wire bytes (the frame `Node::publish`
/// ingests).
fn wire_bytes(ev: &SignedEvent, dev_sk: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev_sk);
    WireEvent::seal(csb, sig).to_bytes()
}

/// Build a `room.created` genesis event; returns (`room_id`, `event_id`, bytes).
fn mk_genesis(admin: &Actor) -> (RoomId, EventId, Vec<u8>) {
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
            room_name: "ceil-e2e".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: binding,
        }),
    };
    (room_id, ev.event_id(), wire_bytes(&ev, &admin.dev_sk))
}

/// Build a `member.invited` event; `n` seeds `invite_id`/secret to keep the
/// matching [`mk_join`] in lockstep.
fn mk_invite(
    admin: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    invitee: IdentityKey,
    n: u8,
    ts: u64,
) -> (EventId, Vec<u8>) {
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
    (ev.event_id(), wire_bytes(&ev, &admin.dev_sk))
}

/// Build a `member.joined` event; `n` must match the corresponding invite.
fn mk_join(
    member: &Actor,
    room_id: RoomId,
    prev: &[EventId],
    n: u8,
    ts: u64,
) -> (EventId, Vec<u8>) {
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
    (ev.event_id(), wire_bytes(&ev, &member.dev_sk))
}

// ---------------------------------------------------------------------------
// Recording audit sink — captures `active_member_threshold_reached` callbacks.
// ---------------------------------------------------------------------------

/// `(room_id, active, max, remaining)` for each near-cap callback observed.
type NearCapCall = (RoomId, usize, usize, usize);

#[derive(Default, Clone)]
struct RecordingAudit {
    near_cap_calls: Arc<Mutex<Vec<NearCapCall>>>,
}

impl RecordingAudit {
    fn calls(&self) -> Vec<NearCapCall> {
        self.near_cap_calls.lock().expect("audit lock").clone()
    }
}

impl AuditSink for RecordingAudit {
    fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {}
    fn rejected(&self, _device: EndpointId, _cause: RejectCause) {}
    fn connected(&self, _device: EndpointId) {}
    fn disconnected(&self, _device: EndpointId) {}
    fn active_member_threshold_reached(
        &self,
        room_id: &RoomId,
        active: usize,
        max: usize,
        remaining: usize,
    ) {
        self.near_cap_calls
            .lock()
            .expect("audit lock")
            .push((*room_id, active, max, remaining));
    }
}

// ---------------------------------------------------------------------------
// Node-spawning helpers
// ---------------------------------------------------------------------------

/// Open an in-memory `SyncEngine` for `room_id` and seed it with `events` in
/// order.
fn make_engine(room_id: RoomId, events: &[Vec<u8>]) -> SyncEngine {
    let store = EventStore::open_in_memory().expect("in-memory store");
    let mut engine = SyncEngine::open(store, room_id, SyncConfig::default()).expect("open engine");
    for ev in events {
        engine.publish(ev).expect("seed event");
    }
    engine
}

/// Spawn a **managed-room** node via `Node::spawn_room` with a caller-supplied
/// audit sink. No `addr_hint`s are supplied: the warning is a local observer
/// decision over the engine fold and does not depend on any peer link being up,
/// so the test stays single-node and deterministic.
///
/// Returns a boxed future: `Node::spawn_room`'s state machine is ~16 KB, which
/// would otherwise be inlined into every caller and trip clippy `large_futures`.
fn spawn_room_with_audit(
    actor: &Actor,
    engine: SyncEngine,
    audit: Arc<dyn AuditSink>,
) -> Pin<Box<dyn Future<Output = Node> + Send + '_>> {
    Box::pin(async move {
        let cell = Arc::new(Mutex::new(AdmissionView::empty()));
        let admission = Arc::new(SnapshotAdmission::new(cell.clone()));
        Node::spawn_room(
            actor.iroh_secret(),
            admission,
            audit,
            engine,
            NetConfig {
                mode: NetMode::Loopback,
                ..NetConfig::default()
            },
            DEFAULT_TICK,
            vec![],
            cell,
            None,
        )
        .await
        .expect("spawn_room")
    })
}

// ---------------------------------------------------------------------------
// AC1: a live 3 → 4 fold transition emits exactly one near-cap warning
// ===========================================================================

/// AC1 (issue #144): a managed room at 3 active members (admin + two joined
/// invitees) that grows to 4 must emit exactly one
/// `active_member_threshold_reached` callback with the expected counts, and
/// must **not** re-emit on subsequent ticks or forced reconciles (one-shot per
/// crossing, not per-tick spam).
///
/// The boundary under test is the live pump path:
/// `Node::publish` → engine ingest → `RoomReconciler::maybe_reconcile` →
/// `AuditSink::active_member_threshold_reached`. The pure
/// `active_member_warning_crossed` helper is unit-tested in `model.rs`; this
/// test proves the pump actually calls it against each new snapshot and tracks
/// `last_active_member_count` across ticks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_room_warns_once_when_active_count_crosses_threshold() {
    let admin = Actor::new(41);
    let m1 = Actor::new(42);
    let m2 = Actor::new(43);
    let m3 = Actor::new(44);

    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (inv1_id, inv1_bytes) = mk_invite(&admin, room_id, &[genesis_id], m1.identity(), 1, T0 + 1);
    let (join1_id, join1_bytes) = mk_join(&m1, room_id, &[inv1_id], 1, T0 + 2);
    let (inv2_id, inv2_bytes) = mk_invite(&admin, room_id, &[join1_id], m2.identity(), 2, T0 + 3);
    let (join2_id, join2_bytes) = mk_join(&m2, room_id, &[inv2_id], 2, T0 + 4);

    // Seed the engine at 3 active members (admin + m1 + m2) — strictly below
    // the `ACTIVE_MEMBER_WARNING_THRESHOLD` (4).
    let engine = make_engine(
        room_id,
        &[
            genesis_bytes.clone(),
            inv1_bytes.clone(),
            join1_bytes.clone(),
            inv2_bytes.clone(),
            join2_bytes.clone(),
        ],
    );
    assert_eq!(engine.snapshot().active_member_count(), 3);
    assert_eq!(ACTIVE_MEMBER_WARNING_THRESHOLD, 4);

    let audit = Arc::new(RecordingAudit::default());
    let node = spawn_room_with_audit(&admin, engine, audit.clone()).await;

    // Force an initial reconcile and confirm no warning fires for a below-
    // threshold starting snapshot (previous=None → Some(3); no crossing).
    node.reconcile_now().await.expect("initial reconcile");
    assert_eq!(
        audit.calls().len(),
        0,
        "a below-threshold room must not emit a near-cap warning"
    );

    // Publish the third invitee's invite + join. After each `publish` returns
    // the pump has already run `maybe_reconcile` synchronously, so the join's
    // return guarantees the 3 → 4 crossing has been observed.
    let (inv3_id, inv3_bytes) = mk_invite(&admin, room_id, &[join2_id], m3.identity(), 3, T0 + 5);
    node.publish(inv3_bytes).await.expect("publish invite #3");
    // An invite alone does not change active count (invitee is `Invited`, not
    // `Active`); no warning yet.
    assert_eq!(
        audit.calls().len(),
        0,
        "an accepted invite must not trip the active-count threshold"
    );

    let (_join3_id, join3_bytes) = mk_join(&m3, room_id, &[inv3_id], 3, T0 + 6);
    node.publish(join3_bytes).await.expect("publish join #3");

    // The fold is now at 4 active — exactly one crossing callback must have
    // fired, carrying the room id and the expected counts.
    let snapshot = node.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.active_member_count(), 4);
    let calls = audit.calls();
    assert_eq!(
        calls.len(),
        1,
        "the 3 → 4 crossing must emit exactly one near-cap warning (no per-tick spam)"
    );
    assert_eq!(calls[0].0, room_id, "callback must carry the room id");
    assert_eq!(calls[0].1, 4, "active count");
    assert_eq!(calls[0].2, MAX_ACTIVE_MEMBERS, "max (hard cap)");
    assert_eq!(calls[0].3, 1, "remaining headroom");

    // Repeated forced reconciles must not re-emit while the room stays at 4
    // (the one-shot-per-crossing guarantee, independent of admission diff
    // cadence — `force_reconcile` resets the admission detector but not
    // `last_active_member_count`).
    for _ in 0..3 {
        node.reconcile_now().await.expect("repeat reconcile");
    }
    assert_eq!(
        audit.calls().len(),
        1,
        "stay-at-threshold reconciles must not re-warn"
    );

    // Let at least two natural anti-entropy ticks fire so the tick-driven
    // `maybe_reconcile` path is exercised too, then re-assert no spam.
    tokio::time::sleep(DEFAULT_TICK * 3).await;
    assert_eq!(
        audit.calls().len(),
        1,
        "natural tick-driven reconciles must not re-warn while at threshold"
    );

    node.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// OQ-1: a node that starts already at/above the threshold does not warn
// ===========================================================================

/// OQ-1 (spec §4 D3): a node whose **first** observed snapshot is already at
/// the soft threshold emits no initial near-cap warning — the recommended
/// default, because no below-to-threshold crossing was observed in this process
/// and `room members --status` is the current-state surface. Guards the
/// `previous = None` branch of `active_member_warning_crossed` through the live
/// reconciler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_room_does_not_warn_on_startup_already_at_threshold() {
    let admin = Actor::new(51);
    let m1 = Actor::new(52);
    let m2 = Actor::new(53);
    let m3 = Actor::new(54);

    let (room_id, genesis_id, genesis_bytes) = mk_genesis(&admin);
    let (inv1_id, inv1_bytes) = mk_invite(&admin, room_id, &[genesis_id], m1.identity(), 1, T0 + 1);
    let (join1_id, join1_bytes) = mk_join(&m1, room_id, &[inv1_id], 1, T0 + 2);
    let (inv2_id, inv2_bytes) = mk_invite(&admin, room_id, &[join1_id], m2.identity(), 2, T0 + 3);
    let (join2_id, join2_bytes) = mk_join(&m2, room_id, &[inv2_id], 2, T0 + 4);
    let (inv3_id, inv3_bytes) = mk_invite(&admin, room_id, &[join2_id], m3.identity(), 3, T0 + 5);
    let (_join3_id, join3_bytes) = mk_join(&m3, room_id, &[inv3_id], 3, T0 + 6);

    // Seed at exactly the threshold: 4 active members.
    let engine = make_engine(
        room_id,
        &[
            genesis_bytes,
            inv1_bytes,
            join1_bytes,
            inv2_bytes,
            join2_bytes,
            inv3_bytes,
            join3_bytes,
        ],
    );
    assert_eq!(
        engine.snapshot().active_member_count(),
        ACTIVE_MEMBER_WARNING_THRESHOLD
    );

    let audit = Arc::new(RecordingAudit::default());
    let node = spawn_room_with_audit(&admin, engine, audit.clone()).await;

    node.reconcile_now().await.expect("initial reconcile");
    // Let a couple of natural ticks fire as well.
    tokio::time::sleep(DEFAULT_TICK * 2).await;

    assert_eq!(
        audit.calls().len(),
        0,
        "a node that starts at threshold must not emit an initial warning (OQ-1)"
    );

    node.shutdown().await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// Sanity: the threshold and cap are what this test file assumes. If these ever
// change, the tests above must be re-read for correctness (the protocol
// invariant is intentionally not configurable).
// ---------------------------------------------------------------------------

#[test]
fn warning_threshold_is_one_below_the_hard_cap() {
    assert_eq!(MAX_ACTIVE_MEMBERS, 5);
    assert_eq!(ACTIVE_MEMBER_WARNING_THRESHOLD, MAX_ACTIVE_MEMBERS - 1);
}
