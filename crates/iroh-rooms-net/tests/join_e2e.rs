//! End-to-end join-by-ticket integration tests (IR-0104).
//!
//! Exercises the full join-bootstrap flow at the [`Node`] API layer using two
//! in-process nodes over `NetMode::Loopback`:
//!
//! ```text
//! Admin: genesis + invite ──► JoinBootstrapAdmission (accept_joins=true)
//! Joiner: (empty store)    ──► dials Admin ──► pulls membership sub-DAG
//!                                          ──► builds + publishes member.joined
//!                                          ──► both peers fold → Active
//! ```
//!
//! Coverage map (issue acceptance criteria):
//!
//! * **AC1 / AC5** — `valid_join_both_peers_show_joiner_active`: a correctly-built
//!   join is accepted; both Admin's and Joiner's snapshots show the joiner `Active`.
//! * **AC3** — `bad_capability_secret_join_not_accepted`: a join with the wrong
//!   `capability_secret` is stored (stateless-valid) but the fold's `gate_join`
//!   rejects it with `BadCapability`; neither peer shows the joiner `Active`.
//! * **AC4** — `expired_invite_join_not_accepted`: a join whose `created_at`
//!   exceeds the invite's `expires_at` is rejected deterministically with
//!   `ExpiredInvite` on both peers (log-only, clock-independent).
//!
//! AC2 (wrong identity key) is pre-IO and is covered by the CLI unit tests
//! (`join_cli.rs`) and by the membership-fold tests (`membership_fold.rs`).
//!
//! ## IR-0207 (issue #32) — agent-role mirrors
//!
//! `gate_join` never branches on `role`, so `agent_bad_capability_secret_join_not_accepted`
//! and `agent_expired_invite_join_not_accepted` re-run AC3/AC4 with the invite and
//! join built at `role = "agent"` (`build_admin_room`/`build_member_joined`'s `role`
//! flipped, everything else identical) — the Node-layer half of IR-0207's "agent join
//! is rejected without valid capability", pinning that an agent gets no different
//! capability verification than a human. The CLI-surface half (corrupt/truncated/
//! wrong-identity agent tickets, which fail pre-IO) lives in
//! `iroh-rooms-cli/tests/agent_invite_flow.rs`; these two are the online cases that
//! require a live admin's fold to render a verdict.
//!
//! Every await is bounded (via `wait_until_contains` / a manual `timeout`) so a
//! wiring bug fails fast rather than hanging CI (mirrors `message_e2e` / `pipe_e2e`).
//!
//! ## Issue #88 — `JoinBootstrapAdmission::new_dynamic` (no-respawn window flip)
//!
//! `dynamic_accept_joins_flips_join_window_without_respawn_or_conn_churn` covers the
//! issue's acceptance sketch end-to-end: an admin built with `new_dynamic` and a
//! shared `Arc<AtomicBool>` window refuses a fresh joiner while the window is
//! closed, opens it on `window.store(true)` (invite mint) **without respawning the
//! admin's `Node`**, completes the joiner's bootstrap + `member.joined` (both peers
//! fold `Active`, reusing the AC1/AC5 convergence check above), closes it again on
//! `window.store(false)` (redemption) so a second unknown device is refused, and
//! asserts a pre-connected resident member's admin-side `ConnEvent` stream carries
//! only its initial admit — no churn attributable to either flip.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use iroh::SecretKey;
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_member_joined;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, MemberJoined, MessageText, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    AllowlistAdmission, BootstrapProof, JoinBootstrapAdmission, NetConfig, NetMode, Node,
    PeerConnState, TracingAudit, DEFAULT_TICK,
};

// ── fixed test fixtures ───────────────────────────────────────────────────────

/// Loopback-wait budget — generous so transient scheduling hiccups don't flake.
const WAIT: Duration = Duration::from_secs(15);

/// Bounded wait for an outcome that must NOT happen (a closed-window refusal is a
/// structural reject decided before any event byte is read, so it never becomes
/// true no matter how long we wait — this only bounds how long the test spends
/// confirming that).
const REFUSAL_WAIT: Duration = Duration::from_secs(3);

/// Room nonce (deterministic across all tests in this file).
const NONCE: [u8; 16] = [0xab; 16];

/// Base timestamp (advisory wall-clock anchor; expiry comparisons use offsets).
const T0: u64 = 1_750_000_000_000;

/// 16-byte identifier embedded in the `MemberInvited.invite_id` field.
/// Passed as `via_invite_id` to `build_member_joined`.
const INV_ID: [u8; 16] = [0x01u8; 16];

/// Correct capability secret — matches the invite's `capability_hash`.
const INV_SECRET: [u8; 16] = [0x42u8; 16];

/// Wrong capability secret — does NOT match `capability_hash(room, INV_ID, INV_SECRET)`.
const WRONG_SECRET: [u8; 16] = [0xFF; 16];

// ── Principal ────────────────────────────────────────────────────────────────

/// A deterministic (identity, device) key pair seeded from a single byte.
/// `endpoint_id() == device_key()` byte-for-byte (Membership §1 / spec A2).
struct Principal {
    id: SigningKey,
    dev: SigningKey,
}

impl Principal {
    fn new(seed: u8) -> Self {
        Self {
            id: SigningKey::from_seed(&[seed; 32]),
            dev: SigningKey::from_seed(&[seed.wrapping_add(0x80); 32]),
        }
    }

    fn identity(&self) -> IdentityKey {
        self.id.identity_key()
    }

    fn device_key(&self) -> iroh_rooms_core::event::keys::DeviceKey {
        self.dev.device_key()
    }

    /// iroh [`SecretKey`] for the transport (== the device signing key, spec A2).
    fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.dev.to_seed())
    }

    /// Authenticated transport identity == `device_key` bytes (spec A2).
    fn endpoint_id(&self) -> iroh::EndpointId {
        self.iroh_secret().public()
    }
}

// ── room-building helpers ─────────────────────────────────────────────────────

/// Serialize + sign a `SignedEvent` into verbatim `WireEvent` bytes.
fn wire(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, dev);
    WireEvent::seal(csb, sig).to_bytes()
}

/// Everything the tests need from `build_admin_room`.
struct RoomSetup {
    room_id: RoomId,
    genesis_event_id: EventId,
    invite_dag_id: EventId,
    /// Wire bytes to seed the admin's engine: `[genesis, invite]`.
    log: Vec<Vec<u8>>,
}

/// Build the admin's room: genesis (admin = admin) + a single `member.invited`
/// for `joiner_identity` with the given `role`. `expires_at` is `None` for the
/// normal case and `Some(T0 + 1)` for the expiry test.
fn build_admin_room(
    admin: &Principal,
    joiner_identity: IdentityKey,
    role: &str,
    expires_at: Option<u64>,
) -> RoomSetup {
    let room_id = signed::derive_room_id(&admin.identity(), &NONCE, T0);

    let genesis_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::RoomCreated,
        created_at: T0,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Join E2E Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: DeviceBinding::create(&room_id, &admin.id, admin.device_key()),
        }),
    };
    let genesis_event_id = genesis_ev.event_id();
    let genesis_bytes = wire(&genesis_ev, &admin.dev);

    let invite_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::MemberInvited,
        created_at: T0 + 1,
        prev_events: vec![genesis_event_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id: INV_ID,
            capability_hash: capability_hash(&room_id, &INV_ID, &INV_SECRET),
            role: role.to_owned(),
            invitee_key: joiner_identity,
            expires_at,
            invitee_hint: None,
        }),
    };
    let invite_dag_id = invite_ev.event_id();
    let invite_bytes = wire(&invite_ev, &admin.dev);

    RoomSetup {
        room_id,
        genesis_event_id,
        invite_dag_id,
        log: vec![genesis_bytes, invite_bytes],
    }
}

/// Resident member bob's invite id/secret (distinct from the joiner's `INV_ID`).
const BOB_INV_ID: [u8; 16] = [0x02u8; 16];
const BOB_SECRET: [u8; 16] = [0x43u8; 16];

/// Build the admin's room WITH a conversation: genesis → invite(resident) →
/// join(resident) → a linear `n_chat`-message resident-authored chain → the
/// joiner's invite citing the **chat tip** — exactly the shape `room invite`
/// mints once a conversation has started (`prev_events = current DAG heads`).
///
/// With `n_chat > max_backfill_depth` (64), the joiner's bootstrap can only
/// succeed if the membership pull itself serves the causally-closed ancestry:
/// a provisional peer gets no `WantEvents` backfill, and even a permitted
/// chase would exceed the depth bound.
#[allow(clippy::too_many_lines)] // one linear event-by-event fixture; splitting obscures the shape
fn build_admin_room_with_history(
    admin: &Principal,
    resident: &Principal,
    joiner_identity: IdentityKey,
    n_chat: u32,
) -> RoomSetup {
    let room_id = signed::derive_room_id(&admin.identity(), &NONCE, T0);
    let mut log = Vec::new();
    let mut t = T0;

    let genesis_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::RoomCreated,
        created_at: t,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "Join After History Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: DeviceBinding::create(&room_id, &admin.id, admin.device_key()),
        }),
    };
    let genesis_event_id = genesis_ev.event_id();
    log.push(wire(&genesis_ev, &admin.dev));

    t += 1;
    let inv_resident = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::MemberInvited,
        created_at: t,
        prev_events: vec![genesis_event_id],
        content: Content::MemberInvited(MemberInvited {
            invite_id: BOB_INV_ID,
            capability_hash: capability_hash(&room_id, &BOB_INV_ID, &BOB_SECRET),
            role: "member".to_owned(),
            invitee_key: resident.identity(),
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let inv_resident_id = inv_resident.event_id();
    log.push(wire(&inv_resident, &admin.dev));

    t += 1;
    let join_resident = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: resident.identity(),
        device_id: resident.device_key(),
        event_type: EventType::MemberJoined,
        created_at: t,
        prev_events: vec![inv_resident_id],
        content: Content::MemberJoined(MemberJoined {
            via_invite_id: BOB_INV_ID,
            capability_secret: BOB_SECRET,
            role: "member".to_owned(),
            device_binding: DeviceBinding::create(&room_id, &resident.id, resident.device_key()),
            display_name: None,
        }),
    };
    let mut tip = join_resident.event_id();
    log.push(wire(&join_resident, &resident.dev));

    for i in 0..n_chat {
        t += 1;
        let msg = SignedEvent {
            schema_version: 1,
            room_id,
            sender_id: resident.identity(),
            device_id: resident.device_key(),
            event_type: EventType::MessageText,
            created_at: t,
            prev_events: vec![tip],
            content: Content::MessageText(MessageText {
                body: format!("history msg {i}"),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        tip = msg.event_id();
        log.push(wire(&msg, &resident.dev));
    }

    t += 1;
    let invite_ev = SignedEvent {
        schema_version: 1,
        room_id,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::MemberInvited,
        created_at: t,
        prev_events: vec![tip],
        content: Content::MemberInvited(MemberInvited {
            invite_id: INV_ID,
            capability_hash: capability_hash(&room_id, &INV_ID, &INV_SECRET),
            role: "member".to_owned(),
            invitee_key: joiner_identity,
            expires_at: None,
            invitee_hint: None,
        }),
    };
    let invite_dag_id = invite_ev.event_id();
    log.push(wire(&invite_ev, &admin.dev));

    RoomSetup {
        room_id,
        genesis_event_id,
        invite_dag_id,
        log,
    }
}

// ── node-spawning helpers ─────────────────────────────────────────────────────

/// Spawn an admin node pre-seeded with `log` (genesis + invite).
/// Uses [`JoinBootstrapAdmission`] with `accept_joins=true` so a first-time
/// joiner whose device is unknown is admitted provisionally.
async fn spawn_admin_node(admin: &Principal, room: RoomId, log: &[Vec<u8>]) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory admin store");
    let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("admin engine");
    for ev in log {
        engine.publish(ev).expect("seed admin event");
    }
    // An empty inner AllowlistAdmission: the joiner's device is not yet bound (it
    // is unknown → provisionally admitted while `accept_joins` is true).
    let admission = JoinBootstrapAdmission::new(AllowlistAdmission::new(), true);
    let cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    Node::spawn(
        admin.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .expect("spawn admin node")
}

/// Spawn an admin node pre-seeded with `log`, using
/// [`JoinBootstrapAdmission::new_dynamic`] (issue #88) instead of a fixed
/// `accept_joins` bool. The inner `AllowlistAdmission` already knows `resident`
/// as an Active member, so `resident`'s admit verdict never depends on
/// `window`; only a genuinely unknown device's outcome tracks the live flag.
async fn spawn_admin_node_dynamic(
    admin: &Principal,
    resident: &Principal,
    room: RoomId,
    log: &[Vec<u8>],
    window: Arc<AtomicBool>,
) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory admin store");
    let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("admin engine");
    for ev in log {
        engine.publish(ev).expect("seed admin event");
    }
    let inner = AllowlistAdmission::new()
        .bind_device(resident.endpoint_id(), resident.identity())
        .set_active(resident.identity());
    let admission = JoinBootstrapAdmission::new_dynamic(inner, window);
    let cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    Node::spawn(
        admin.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .expect("spawn admin node (dynamic admission)")
}

/// Spawn a joiner node with an empty store that presents a join-bootstrap
/// capability proof on connect (issue #112) for `invite_id` + `secret`. Uses an
/// [`AllowlistAdmission`] that admits the admin (so the outbound dial succeeds and
/// the sub-DAG pull from the admin can proceed). A genuine invitee passes the
/// invite it holds; the admin serves it the membership closure only once the proof
/// matches an on-log invite.
async fn spawn_joiner_node(
    joiner: &Principal,
    admin: &Principal,
    room: RoomId,
    invite_id: [u8; 16],
    secret: [u8; 16],
) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory joiner store");
    let engine = SyncEngine::open(store, room, SyncConfig::default()).expect("joiner engine");
    // Joiner must admit the admin so the dial_loop's authorization check passes.
    let admission = AllowlistAdmission::new()
        .bind_device(admin.endpoint_id(), admin.identity())
        .set_active(admin.identity());
    let cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    Node::spawn_join_bootstrap(
        joiner.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
        BootstrapProof {
            room_id: room,
            invite_id,
            capability_secret: secret,
        },
    )
    .await
    .expect("spawn joiner node")
}

/// Spawn a joiner node that presents **no** capability proof (a plain
/// [`Node::spawn`]) — used to model an uninvited dialer for the issue #112/#121
/// security regressions. It still admits the admin so the dial itself succeeds.
/// `log` pre-seeds its store: empty for a dialer that knows only the room id, or
/// a copy of the room log for a worst-case attacker holding stale history
/// out-of-band (so any leaked live frame folds cleanly and shows in its store).
async fn spawn_bare_joiner_node(
    joiner: &Principal,
    admin: &Principal,
    room: RoomId,
    log: &[Vec<u8>],
) -> Node {
    let store = EventStore::open_in_memory().expect("in-memory joiner store");
    let mut engine = SyncEngine::open(store, room, SyncConfig::default()).expect("joiner engine");
    for ev in log {
        engine.publish(ev).expect("seed bare joiner event");
    }
    let admission = AllowlistAdmission::new()
        .bind_device(admin.endpoint_id(), admin.identity())
        .set_active(admin.identity());
    let cfg = NetConfig {
        mode: NetMode::Loopback,
        ..NetConfig::default()
    };
    Node::spawn(
        joiner.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .expect("spawn bare joiner node")
}

// ── wait helper ───────────────────────────────────────────────────────────────

/// Poll `node.snapshot()` until `identity` is Active or `deadline` elapses.
/// Returns `true` on Active, `false` on timeout.
async fn wait_active(node: &Node, identity: &IdentityKey, deadline: Duration) -> bool {
    tokio::time::timeout(deadline, async {
        loop {
            if let Ok(snap) = node.snapshot().await {
                if snap.is_active(identity) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok()
}

/// Publish `join_wire` on `joiner_node` and wait until `admin_node`'s store also
/// contains the join. Returns the event id. Panics on publish or wait failure.
async fn publish_and_wait_admin(
    joiner_node: &Node,
    admin_node: &Node,
    join_wire: WireEvent,
) -> EventId {
    let join_id: EventId = join_wire.id.parse().expect("valid join event_id");
    joiner_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined");
    admin_node
        .wait_until_contains(join_id, WAIT)
        .await
        .expect("admin received the join frame");
    join_id
}

// ── AC1 / AC5 — valid join ───────────────────────────────────────────────────

/// AC1 / AC5: A correctly-built `member.joined` is accepted by the fold on
/// **both** peers.  After the join propagates:
///
/// * the admin's snapshot shows the joiner `Active` (it received and folded the join)
/// * the joiner's own snapshot shows itself `Active` (it published locally)
///
/// This is the headline two-peer integration test specified in §11.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_join_both_peers_show_joiner_active() {
    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);

    let setup = build_admin_room(&admin, joiner.identity(), "member", None);

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;

    // Joiner dials Admin; Admin admits it provisionally (first-time device +
    // open invite window → `AdmitProvisional`). The engine on both sides calls
    // `on_connect`, which triggers `WantMembership`. Admin serves the membership
    // sub-DAG (genesis + invite) to Joiner.
    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    // Wait for the sub-DAG to land in Joiner's store before building the join.
    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("joiner pulled genesis from admin (WantMembership pull)");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled invite from admin");

    // Joiner now resolves as Invited in the fold. Build the join from its current
    // DAG heads (which descend from the invite — spec D7).
    let heads = joiner_node.heads().await.expect("joiner DAG heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET,
        "member",
        binding,
        Some("Joiner"),
        &heads,
        T0 + 1_000,
    );

    let join_id = publish_and_wait_admin(&joiner_node, &admin_node, join_wire).await;

    // AC5 — Admin's snapshot: joiner is Active (fold accepted the join, the
    // `upgrade-on-learn` path cleared the provisional restriction).
    assert!(
        wait_active(&admin_node, &joiner.identity(), WAIT).await,
        "AC5: admin's snapshot must show the joiner as Active after the join is folded"
    );

    // AC1 — Joiner's own snapshot: also Active (it published the join locally).
    assert!(
        wait_active(&joiner_node, &joiner.identity(), WAIT).await,
        "AC1: joiner's own snapshot must show itself as Active"
    );

    // Sanity: joiner's store also has the join (it was the publisher).
    assert!(
        joiner_node
            .store_contains(join_id)
            .await
            .expect("store_contains"),
        "joiner's store must contain the join it published"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

// ── join-after-history regression ────────────────────────────────────────────

/// Regression for "no one can join a room once a conversation has started": an
/// invite minted after chat cites the chat tip as its `prev_events`; the
/// provisionally-admitted joiner can only pull via `WantMembership` (its
/// `WantEvents`/`WantRecentChat` are dropped by the join bootstrap), so the
/// membership response must be causally closed for the invite to ever classify.
/// The 70-deep member-authored chain also exceeds `max_backfill_depth` (64).
/// Before the closure fix, the `wait_until_contains(invite)` below deadlocked
/// until timeout and the join failed with "could not bootstrap the room
/// membership".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_succeeds_after_conversation_has_started() {
    let admin = Principal::new(0x01);
    let resident = Principal::new(0x30);
    let joiner = Principal::new(0x10);

    let setup = build_admin_room_with_history(&admin, &resident, joiner.identity(), 70);

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;

    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    // The load-bearing wait: the joiner must obtain its naming invite — whose
    // ancestry runs through the entire conversation — from the membership pull
    // alone, while still provisional.
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled the post-conversation invite (causally-closed WantMembership)");

    let heads = joiner_node.heads().await.expect("joiner DAG heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET,
        "member",
        binding,
        Some("Late Joiner"),
        &heads,
        T0 + 1_000,
    );

    publish_and_wait_admin(&joiner_node, &admin_node, join_wire).await;

    assert!(
        wait_active(&admin_node, &joiner.identity(), WAIT).await,
        "admin's snapshot must show the late joiner Active after a room with history"
    );
    assert!(
        wait_active(&joiner_node, &joiner.identity(), WAIT).await,
        "late joiner's own snapshot must show itself Active"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

// ── AC3 — bad capability secret ──────────────────────────────────────────────

/// AC3: A `member.joined` with the wrong `capability_secret` passes stateless
/// validation (it is a well-formed event with a valid signature) but the fold's
/// `gate_join` rejects it with `BadCapability` because the recomputed
/// `BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ wrong_secret)` does not
/// match the invite's stored `capability_hash`.
///
/// The fold rejects the event and does NOT store it on the publisher or fan it
/// out to peers. Neither peer's membership snapshot transitions the joiner to
/// `Active`. The `node.publish()` API call still returns `Ok` because stateless
/// validation passes; only the fold verdict is decisive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_capability_secret_join_not_accepted() {
    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);

    let setup = build_admin_room(&admin, joiner.identity(), "member", None);

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;

    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("joiner pulled genesis");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled invite");

    let heads = joiner_node.heads().await.expect("joiner heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    // Build join with the WRONG secret (INV_ID is correct; only the secret differs).
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &WRONG_SECRET, // ← wrong: gate_join rejects BadCapability
        "member",
        binding,
        None,
        &heads,
        T0 + 1_000,
    );

    // node.publish succeeds (stateless-valid); the fold rejects the event with
    // BadCapability and does not store it, so there is no event_id to wait for
    // in either peer's store.
    joiner_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined");

    // Allow a brief settling window so any in-flight engine work completes.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // AC3: admin's fold must not show the joiner as Active (gate_join → BadCapability).
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    assert!(
        !admin_snap.is_active(&joiner.identity()),
        "AC3: admin must NOT show joiner as Active when capability_secret is wrong"
    );

    // AC3: joiner's own fold reaches the same deterministic verdict.
    let joiner_snap = joiner_node.snapshot().await.expect("joiner snapshot");
    assert!(
        !joiner_snap.is_active(&joiner.identity()),
        "AC3: joiner must NOT show itself as Active when its own secret is wrong"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

/// IR-0207 (#32) AC3 — the `agent`-role mirror of `bad_capability_secret_join_not_accepted`.
/// Identical to the test above except the invite and the join are both built at
/// `role = "agent"` instead of `"member"`: since `gate_join` computes
/// `capability_hash` and compares it without ever inspecting `role`, an agent's
/// wrong-secret join is rejected by the exact same `BadCapability` verdict a
/// human's is — proving "the same capability verification as a human peer" at the
/// Node layer, not just asserting it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_bad_capability_secret_join_not_accepted() {
    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);

    let setup = build_admin_room(&admin, joiner.identity(), "agent", None);

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;

    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("joiner pulled genesis");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled invite");

    let heads = joiner_node.heads().await.expect("joiner heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    // Build join with the WRONG secret (INV_ID is correct; only the secret differs).
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &WRONG_SECRET, // ← wrong: gate_join rejects BadCapability
        "agent",
        binding,
        None,
        &heads,
        T0 + 1_000,
    );

    // node.publish succeeds (stateless-valid); the fold rejects the event with
    // BadCapability and does not store it, so there is no event_id to wait for
    // in either peer's store.
    joiner_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined");

    // Allow a brief settling window so any in-flight engine work completes.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // AC3: admin's fold must not show the agent as Active (gate_join → BadCapability).
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    assert!(
        !admin_snap.is_active(&joiner.identity()),
        "IR-0207 AC3: admin must NOT show the agent as Active when capability_secret is wrong"
    );

    // AC3: joiner's own fold reaches the same deterministic verdict.
    let joiner_snap = joiner_node.snapshot().await.expect("joiner snapshot");
    assert!(
        !joiner_snap.is_active(&joiner.identity()),
        "IR-0207 AC3: agent must NOT show itself as Active when its own secret is wrong"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

// ── AC4 — expired invite ─────────────────────────────────────────────────────

/// AC4: An invite with `expires_at = T0 + 1` (set before the join's `created_at
/// = T0 + 1_000`) is rejected by the joiner's own fold with `ExpiredInvite`
/// (`join.created_at > invite.expires_at`) before the event is stored or fanned
/// out. The admin therefore never receives the frame, and neither peer's snapshot
/// shows the joiner as `Active`.
///
/// Expiry is log-only and clock-independent (spec D8 / §6 "Expiry determinism"):
/// the verdict depends only on signed timestamps in the DAG, never on wall-clock
/// time, so the outcome would be convergent on any peer that did receive the event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_invite_join_not_accepted() {
    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);

    // Invite expires at T0+1; the join will have created_at = T0+1_000 (after expiry).
    let setup = build_admin_room(&admin, joiner.identity(), "member", Some(T0 + 1));

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;

    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("joiner pulled genesis");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled (expired) invite");

    let heads = joiner_node.heads().await.expect("joiner heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    // join.created_at = T0+1_000 > invite.expires_at = T0+1 → ExpiredInvite.
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET, // correct secret; expiry alone rejects it
        "member",
        binding,
        None,
        &heads,
        T0 + 1_000, // ← after invite.expires_at (T0+1)
    );

    // node.publish succeeds (stateless-valid); the fold rejects the event with
    // ExpiredInvite and does not store it, so there is no event_id to wait for.
    joiner_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // AC4: admin's fold rejects the join with ExpiredInvite; joiner not Active.
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    assert!(
        !admin_snap.is_active(&joiner.identity()),
        "AC4: admin must NOT show joiner as Active when the invite is expired"
    );

    // AC4: joiner's fold independently reaches the same verdict (convergent).
    let joiner_snap = joiner_node.snapshot().await.expect("joiner snapshot");
    assert!(
        !joiner_snap.is_active(&joiner.identity()),
        "AC4: joiner must NOT show itself as Active when the invite is expired (deterministic)"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

/// IR-0207 (#32) AC3/AC4 — the `agent`-role mirror of `expired_invite_join_not_accepted`.
/// Identical to the test above except the invite and the join are both built at
/// `role = "agent"` instead of `"member"`: expiry is a log-timestamp comparison
/// (`join.created_at > invite.expires_at`) that never inspects `role`, so an
/// expired agent invite is rejected by the exact same `ExpiredInvite` verdict a
/// human's is. Deterministic (injected timestamps, no sleep/wall-clock read).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_expired_invite_join_not_accepted() {
    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);

    // Invite expires at T0+1; the join will have created_at = T0+1_000 (after expiry).
    let setup = build_admin_room(&admin, joiner.identity(), "agent", Some(T0 + 1));

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;

    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("joiner pulled genesis");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled (expired) invite");

    let heads = joiner_node.heads().await.expect("joiner heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    // join.created_at = T0+1_000 > invite.expires_at = T0+1 → ExpiredInvite.
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET, // correct secret; expiry alone rejects it
        "agent",
        binding,
        None,
        &heads,
        T0 + 1_000, // ← after invite.expires_at (T0+1)
    );

    // node.publish succeeds (stateless-valid); the fold rejects the event with
    // ExpiredInvite and does not store it, so there is no event_id to wait for.
    joiner_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // AC4: admin's fold rejects the join with ExpiredInvite; agent not Active.
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    assert!(
        !admin_snap.is_active(&joiner.identity()),
        "IR-0207 AC4: admin must NOT show the agent as Active when the invite is expired"
    );

    // AC4: joiner's fold independently reaches the same verdict (convergent).
    let joiner_snap = joiner_node.snapshot().await.expect("joiner snapshot");
    assert!(
        !joiner_snap.is_active(&joiner.identity()),
        "IR-0207 AC4: agent must NOT show itself as Active when the invite is expired (deterministic)"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

// ── Bootstrap seam: provisional peer cannot pull non-membership events ────────

/// Security regression: a provisional peer that tries to pull non-membership
/// events (e.g. `WantRecentChat`) should be blocked by the provisional filter in
/// the admin's node pump. The admin's store must not deliver the **windowed chat
/// plane** to an unconfirmed dialer. (Chat that is a structural `prev_events`
/// ancestor of a membership event is different: it rides along the
/// causally-closed `WantMembership` response by design — the fold cannot verify
/// an invite without its ancestry; see `join_succeeds_after_conversation_has_started`.
/// This fixture's chat-free log keeps the two planes disjoint, so the oracle
/// below pins the boundary for chat *outside* the membership ancestry.)
///
/// This tests the `provisional_allows` gate in `node.rs`: only `WantMembership`,
/// `Events`, `AdminTip`, `Heads`, and `NotFound` pass; `WantRecentChat` is dropped.
/// The structural guarantee is that the joiner's chat-pull window does not carry
/// over to a new store: after shutdown, the joiner has only membership events, not
/// any chat frames the admin might have in its store.
///
/// Note: because the engine's `on_connect` does not automatically issue
/// `WantRecentChat` (that is triggered by the CLI's `room tail` path), this
/// test verifies the boundary at the level where a provisional peer's engine
/// happens to receive — or attempt to request — non-membership content. The
/// absence of chat in the joiner's tail after the sub-DAG pull is the oracle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provisional_peer_does_not_receive_chat_events() {
    use iroh_rooms_core::event::content::EventType;

    let admin = Principal::new(0x01);
    let joiner = Principal::new(0x10);

    let setup = build_admin_room(&admin, joiner.identity(), "member", None);

    // Seed the admin with the room log.
    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;

    // Joiner connects (provisionally) but does NOT publish a join — so it stays
    // at most Invited and the provisional restriction is never lifted.
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;
    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    // Wait for the membership pull to complete (genesis + invite reach the joiner).
    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("joiner pulled genesis");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("joiner pulled invite");

    // Give the engine a moment to settle any remaining in-flight messages.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The joiner's tail should contain only membership events (RoomCreated +
    // MemberInvited), never any chat (MessageText) from the admin's store.
    let tail = joiner_node.room_tail(100).await.expect("joiner room_tail");
    let chat_events: Vec<_> = tail
        .iter()
        .filter(|e| e.event_type == EventType::MessageText)
        .collect();
    assert!(
        chat_events.is_empty(),
        "provisional peer must not receive MessageText events from the admin's store; \
         got {} chat events",
        chat_events.len()
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
}

/// Issue #112 (security regression): an **uninvited** dialer that knows only the
/// room id and the admin's address is admitted provisionally while the
/// `--accept-joins` window is open, but — because it presents no capability proof —
/// the admin serves it **nothing**. In particular it does not receive the member
/// chat that entered the membership ancestry (the #111 causally-closed
/// `WantMembership`), which before this fix rode along to any provisional dialer.
/// A genuine invitee that proves possession still bootstraps
/// (`join_succeeds_after_conversation_has_started`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninvited_provisional_dialer_gets_no_membership_closure() {
    let admin = Principal::new(0x01);
    let resident = Principal::new(0x30);
    let joiner = Principal::new(0x10); // the (absent) genuine invitee INV_ID is for
    let attacker = Principal::new(0x40); // knows the room + admin, holds no invite

    // A room whose joiner-invite descends from resident chat, so the membership
    // closure carries that chat (the #111 scenario the #112 gate must contain).
    let setup = build_admin_room_with_history(&admin, &resident, joiner.identity(), 5);
    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;

    // The attacker dials with NO capability proof.
    let attacker_node = spawn_bare_joiner_node(&attacker, &admin, setup.room_id, &[]).await;
    attacker_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    // It must never pull the closure — not even genesis.
    assert!(
        attacker_node
            .wait_until_contains(setup.genesis_event_id, REFUSAL_WAIT)
            .await
            .is_err(),
        "issue #112: an unproven provisional dialer must not pull the membership closure"
    );

    // Settle any in-flight frames, then assert its store holds nothing at all — in
    // particular no leaked `MessageText` chat from the admin's history.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let tail = attacker_node
        .room_tail(200)
        .await
        .expect("attacker room_tail");
    assert!(
        tail.is_empty(),
        "issue #112: an unproven provisional dialer must receive no events; got {} (chat: {})",
        tail.len(),
        tail.iter()
            .filter(|e| e.event_type == EventType::MessageText)
            .count()
    );

    admin_node.shutdown().await.expect("shutdown admin");
    attacker_node.shutdown().await.expect("shutdown attacker");
}

/// Issue #121 (the #112 residual): an **uninvited** dialer admitted provisionally
/// while the `--accept-joins` window is open must receive no **live fan-out**
/// either — before this fix, a provisional accept reaching `Connected` ran
/// `engine.on_connect`, inserting the unproven dialer into the engine's peer set,
/// so `store_and_fanout` pushed every newly accepted event (and every tick
/// advertised the admin tip + heads) to it. The attacker here is worst-case: it
/// already holds a stale out-of-band copy of the room log, so any leaked live
/// push would fold cleanly, making `wait_until_contains` on its store a precise
/// leak oracle (an empty-store attacker's engine would drop the pushed frame at
/// the signer pre-check, masking the wire-level disclosure).
///
/// A genuine invitee that proved possession is connected alongside as the
/// positive control: it must receive the same live publish — which also pins
/// that the deferred handshake actually runs once the capability proof verifies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninvited_provisional_dialer_receives_no_live_fanout() {
    let admin = Principal::new(0x01);
    let resident = Principal::new(0x30);
    let joiner = Principal::new(0x10); // genuine invitee; INV_ID is minted for it
    let attacker = Principal::new(0x40); // knows room + admin + stale log, holds no invite

    let setup = build_admin_room_with_history(&admin, &resident, joiner.identity(), 5);
    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;

    // The attacker dials with NO capability proof and stays connected through
    // the whole open window.
    let attacker_node = spawn_bare_joiner_node(&attacker, &admin, setup.room_id, &setup.log).await;
    attacker_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    admin_node
        .wait_for_state(attacker.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("attacker admitted provisionally (window open)");

    // The genuine invitee connects with its proof and completes the closure pull
    // (it does NOT publish a join — it stays a proven provisional peer). Waiting
    // for the closure tip guarantees the whole causally-ordered response landed.
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;
    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("proven joiner pulled the membership closure");

    // Let the admin's pump settle the joiner's deferred handshake (the proof and
    // the Connected transition arrive on unordered channels) before publishing.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Live chat published while both dialers are connected and the window is open.
    let live_chat = SignedEvent {
        schema_version: 1,
        room_id: setup.room_id,
        sender_id: admin.identity(),
        device_id: admin.device_key(),
        event_type: EventType::MessageText,
        created_at: T0 + 100,
        prev_events: vec![setup.invite_dag_id],
        content: Content::MessageText(MessageText {
            body: "live message during the open join window".to_owned(),
            format: None,
            in_reply_to: None,
            mentions: None,
        }),
    };
    let live_chat_id = live_chat.event_id();
    admin_node
        .publish(wire(&live_chat, &admin.dev))
        .await
        .expect("admin publishes live chat");

    // Positive control: the proven invitee receives the live push (fan-out ran,
    // and the deferred handshake completed on proof verification).
    joiner_node
        .wait_until_contains(live_chat_id, WAIT)
        .await
        .expect("issue #121: a proven provisional peer still receives live fan-out");

    // The unproven attacker must never see it: its handshake stayed deferred, so
    // it never entered the fan-out set — nothing is pushed or advertised to it.
    assert!(
        attacker_node
            .wait_until_contains(live_chat_id, REFUSAL_WAIT)
            .await
            .is_err(),
        "issue #121: an unproven provisional dialer must receive no live fan-out"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    joiner_node.shutdown().await.expect("shutdown joiner");
    attacker_node.shutdown().await.expect("shutdown attacker");
}

// ── Issue #88 — JoinBootstrapAdmission::new_dynamic (no-respawn window flip) ─

/// Issue #88's acceptance sketch, end-to-end: a resident host built with
/// [`JoinBootstrapAdmission::new_dynamic`] refuses join-bootstrap while its
/// shared `Arc<AtomicBool>` window is closed; minting an invite
/// (`window.store(true)`) opens the window **without restarting the admin's
/// `Node`** and a joiner completes the bootstrap + `member.joined`, converging
/// both peers to `Active`; redeeming it (`window.store(false)`) closes the
/// window again and a second unknown device is refused. Throughout, a resident
/// member that connected *before* either flip is asserted to produce no
/// disconnect/reconnect `ConnEvent` on the admin's live stream — the direct
/// consequence of admission being consulted only on the accept path for new
/// inbound connections (`admission.rs`'s `new_dynamic` doc / handler.rs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dynamic_accept_joins_flips_join_window_without_respawn_or_conn_churn() {
    let admin = Principal::new(0x01);
    let resident = Principal::new(0x20);
    let joiner = Principal::new(0x10);
    let second_joiner = Principal::new(0x11);

    let setup = build_admin_room(&admin, joiner.identity(), "member", None);
    let window = Arc::new(AtomicBool::new(false));

    let admin_node =
        spawn_admin_node_dynamic(&admin, &resident, setup.room_id, &setup.log, window.clone())
            .await;

    // A resident member connects once, before any flip, and stays connected for
    // the whole test — the peer whose admin-side `ConnEvent` stream must stay quiet.
    let resident_node =
        spawn_joiner_node(&resident, &admin, setup.room_id, INV_ID, INV_SECRET).await;
    let mut conn_events = admin_node.conn_events();
    resident_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    admin_node
        .wait_for_state(resident.endpoint_id(), PeerConnState::Connected, WAIT)
        .await
        .expect("resident member connects and is admitted (Active, independent of the window)");

    // ── Window closed: a fresh joiner's bootstrap dial is refused ───────────
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;
    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    assert!(
        joiner_node
            .wait_until_contains(setup.genesis_event_id, REFUSAL_WAIT)
            .await
            .is_err(),
        "closed window: joiner must not pull the membership sub-DAG"
    );
    assert!(
        !wait_active(&joiner_node, &joiner.identity(), REFUSAL_WAIT).await,
        "closed window: joiner must not converge to Active"
    );

    // ── Mint: open the window WITHOUT respawning the admin's session ───────
    window.store(true, Ordering::Relaxed);
    // The prior dial loop already stopped after the remote reject (`peer::dial_loop`
    // returns on `Established::RemoteRejected`), so redial now that the window is open.
    joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    joiner_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("open window: joiner pulls genesis from the admin");
    joiner_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("open window: joiner pulls the invite from the admin");

    let heads = joiner_node.heads().await.expect("joiner DAG heads");
    let binding = DeviceBinding::create(&setup.room_id, &joiner.id, joiner.device_key());
    let join_wire = build_member_joined(
        &joiner.id,
        &joiner.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET,
        "member",
        binding,
        Some("Joiner"),
        &heads,
        T0 + 1_000,
    );
    publish_and_wait_admin(&joiner_node, &admin_node, join_wire).await;

    assert!(
        wait_active(&admin_node, &joiner.identity(), WAIT).await,
        "open window: admin's fold must show the joiner Active once the join lands"
    );
    assert!(
        wait_active(&joiner_node, &joiner.identity(), WAIT).await,
        "open window: joiner's own fold must show itself Active"
    );

    // ── Redeem: close the window again WITHOUT respawning ───────────────────
    window.store(false, Ordering::Relaxed);

    let second_joiner_node =
        spawn_joiner_node(&second_joiner, &admin, setup.room_id, INV_ID, INV_SECRET).await;
    second_joiner_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    assert!(
        second_joiner_node
            .wait_until_contains(setup.genesis_event_id, REFUSAL_WAIT)
            .await
            .is_err(),
        "re-closed window: a second unknown device must be refused again"
    );

    // ── No ConnEvent churn on the resident's connection across either flip ──
    let mut resident_events = Vec::new();
    while let Ok(ev) = conn_events.try_recv() {
        if ev.device == resident.endpoint_id() {
            resident_events.push(ev);
        }
    }
    assert_eq!(
        resident_events.len(),
        1,
        "the resident's admin-side ConnEvent stream must carry exactly the initial \
         admit and nothing else across the mint/redeem flips; saw {resident_events:?}"
    );
    assert_eq!(
        (resident_events[0].from, resident_events[0].to),
        (PeerConnState::Connected, PeerConnState::Connected),
        "the resident's only ConnEvent must be the first-sight admit, not a disconnect/reconnect"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    resident_node.shutdown().await.expect("shutdown resident");
    joiner_node.shutdown().await.expect("shutdown joiner");
    second_joiner_node
        .shutdown()
        .await
        .expect("shutdown second_joiner");
}
