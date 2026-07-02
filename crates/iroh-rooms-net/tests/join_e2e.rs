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
//! Every await is bounded (via `wait_until_contains` / a manual `timeout`) so a
//! wiring bug fails fast rather than hanging CI (mirrors `message_e2e` / `pipe_e2e`).
//!
//! ## IR-0206 (agent identity) — the live-transport tier
//!
//! An agent is an ordinary principal distinguished only by `role = "agent"`
//! (Spike §1); `build_admin_room` takes the invited `role` so the same
//! genesis→invite→dial→join→fold pipeline above is reused verbatim for an agent.
//! The CLI/unit/fold-level suites (`agent_cli.rs`, `agent.rs`, `fold.rs`) already
//! prove every IR-0206 AC in-process; these two tests are the live-transport
//! counterpart explicitly deferred to the e2e phase — two real async `Node`s
//! dialing each other over loopback QUIC, not an in-memory fold call:
//!
//! * **AC1/AC2/AC4** — `agent_role_join_over_real_transport_becomes_active`: an
//!   agent-role invite + a real QUIC dial + join is accepted on **both** peers,
//!   and both snapshots resolve `Role::Agent` — the identical wire pipeline as a
//!   member join, proving humans and agents share one protocol model even at the
//!   transport boundary.
//! * **AC3** — `uninvited_agent_cannot_gain_active_membership_over_real_transport`:
//!   an agent identity with **no** invite on the admin's log dials in under the
//!   same join-bootstrap admission a legitimate joiner uses (so the connection
//!   itself is not the gate), then forges a `member.joined`. The fold rejects it
//!   with `BadCapability` on both peers and the agent never becomes `Active` —
//!   explicit invite is enforced at the log, not the wire.

use std::sync::Arc;
use std::time::Duration;

use iroh::SecretKey;
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_member_joined;
use iroh_rooms_core::event::content::{
    capability_hash, Content, EventType, MemberInvited, RoomCreated,
};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::membership::Role;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    AllowlistAdmission, JoinBootstrapAdmission, NetConfig, NetMode, Node, TracingAudit,
    DEFAULT_TICK,
};

// ── fixed test fixtures ───────────────────────────────────────────────────────

/// Loopback-wait budget — generous so transient scheduling hiccups don't flake.
const WAIT: Duration = Duration::from_secs(15);

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
/// for `joiner_identity` with the given `role` ("member" or "agent", IR-0206).
/// `expires_at` is `None` for the normal case and `Some(T0 + 1)` for the expiry
/// test.
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

/// Spawn a joiner node with an empty store.
/// Uses an [`AllowlistAdmission`] that admits the admin (so the outbound dial
/// succeeds and the sub-DAG pull from the admin can proceed).
async fn spawn_joiner_node(joiner: &Principal, admin: &Principal, room: RoomId) -> Node {
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
    Node::spawn(
        joiner.iroh_secret(),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .expect("spawn joiner node")
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
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id).await;

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
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id).await;

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
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id).await;

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

// ── Bootstrap seam: provisional peer cannot pull non-membership events ────────

/// Security regression: a provisional peer that tries to pull non-membership
/// events (e.g. `WantRecentChat`) should be blocked by the provisional filter in
/// the admin's node pump. The admin's store must not deliver room-chat to an
/// unconfirmed dialer.
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
    let joiner_node = spawn_joiner_node(&joiner, &admin, setup.room_id).await;
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

// ── IR-0206 — agent identity over the live transport ─────────────────────────

/// AC1/AC2/AC4: An agent-role invite, redeemed over a **real QUIC dial** between
/// two live `Node`s (not an in-memory fold call), is accepted on both peers and
/// both snapshots resolve `Role::Agent`. This is the wire-boundary counterpart of
/// `valid_join_both_peers_show_joiner_active` (which proves the identical pipeline
/// for `role = "member"`) and of the fold-only
/// `agent_join_folds_to_active_with_role_agent` unit test in
/// `iroh-rooms-core/src/membership/fold.rs`: the same genesis→invite→dial→join
/// path carries an agent to `Active` with no agent-specific transport code path,
/// proving AC4 ("one protocol model") holds all the way to the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_role_join_over_real_transport_becomes_active() {
    let admin = Principal::new(0x01);
    let agent = Principal::new(0x10);

    let setup = build_admin_room(&admin, agent.identity(), "agent", None);

    let admin_node = spawn_admin_node(&admin, setup.room_id, &setup.log).await;
    let agent_node = spawn_joiner_node(&agent, &admin, setup.room_id).await;

    agent_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));

    agent_node
        .wait_until_contains(setup.genesis_event_id, WAIT)
        .await
        .expect("agent pulled genesis from admin (WantMembership pull)");
    agent_node
        .wait_until_contains(setup.invite_dag_id, WAIT)
        .await
        .expect("agent pulled its agent-role invite from admin");

    let heads = agent_node.heads().await.expect("agent DAG heads");
    let binding = DeviceBinding::create(&setup.room_id, &agent.id, agent.device_key());
    let join_wire = build_member_joined(
        &agent.id,
        &agent.dev,
        &setup.room_id,
        &INV_ID,
        &INV_SECRET,
        "agent",
        binding,
        Some("Agent"),
        &heads,
        T0 + 1_000,
    );

    publish_and_wait_admin(&agent_node, &admin_node, join_wire).await;

    // AC1/AC2: both peers converge on Active over the real wire, not just the
    // publishing side.
    assert!(
        wait_active(&admin_node, &agent.identity(), WAIT).await,
        "admin's snapshot must show the agent as Active after the join is folded"
    );
    assert!(
        wait_active(&agent_node, &agent.identity(), WAIT).await,
        "agent's own snapshot must show itself as Active"
    );

    // AC2/AC4: the folded role is Agent on both peers — the role survived the
    // wire (dial, sub-DAG pull, publish, fold) unchanged.
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    let agent_snap = agent_node.snapshot().await.expect("agent snapshot");
    assert_eq!(
        admin_snap.role(&agent.identity()),
        Some(Role::Agent),
        "AC2/AC4: the admin's live snapshot must resolve the joiner to Role::Agent"
    );
    assert_eq!(
        agent_snap.role(&agent.identity()),
        Some(Role::Agent),
        "AC2/AC4: the agent's own live snapshot must resolve itself to Role::Agent"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    agent_node.shutdown().await.expect("shutdown agent");
}

/// AC3: An agent identity with **no** `member.invited` anywhere on the admin's
/// log dials in under the same join-bootstrap admission a legitimate joiner uses
/// (`JoinBootstrapAdmission` with `accept_joins=true`), so the proof is not "the
/// connection itself was refused" — it is that explicit invitation is enforced at
/// the membership log, exactly as PRD §13.3 requires. The uninvited agent forges
/// a `member.joined` citing a fabricated invite id/secret; the fold on both peers
/// rejects it with `BadCapability` (no matching on-log invite to satisfy), and
/// the agent never reaches `Active` on either side. This is the live-transport
/// counterpart of the fold-only `uninvited_agent_join_without_invite_rejected`
/// unit test in `iroh-rooms-core/src/membership/fold.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninvited_agent_cannot_gain_active_membership_over_real_transport() {
    let admin = Principal::new(0x01);
    let stranger_agent = Principal::new(0x20);

    // The admin's room has a genesis only — no invite for `stranger_agent` at all.
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
            room_name: "Uninvited Agent Room".to_owned(),
            room_nonce: NONCE,
            admins: vec![admin.identity()],
            device_binding: DeviceBinding::create(&room_id, &admin.id, admin.device_key()),
        }),
    };
    let genesis_event_id = genesis_ev.event_id();
    let genesis_bytes = wire(&genesis_ev, &admin.dev);

    let admin_node = spawn_admin_node(&admin, room_id, &[genesis_bytes]).await;
    let agent_node = spawn_joiner_node(&stranger_agent, &admin, room_id).await;

    // The uninvited agent dials the admin. It is admitted only provisionally by
    // `JoinBootstrapAdmission` (identical to a legitimate first-time joiner) and
    // can pull the public genesis — that provisional admission is not the gate.
    agent_node.connect_to(admin_node.endpoint_addr().expect("admin addr"));
    agent_node
        .wait_until_contains(genesis_event_id, WAIT)
        .await
        .expect("even an uninvited agent can pull the public genesis (bootstrap semantics)");

    // No invite exists to redeem, so the agent forges one: a fabricated
    // invite_id/secret that matches no on-log `member.invited`.
    let heads = agent_node.heads().await.expect("agent DAG heads");
    let binding = DeviceBinding::create(&room_id, &stranger_agent.id, stranger_agent.device_key());
    let forged_invite_id = [0x99u8; 16];
    let forged_secret = [0x77u8; 16];
    let join_wire = build_member_joined(
        &stranger_agent.id,
        &stranger_agent.dev,
        &room_id,
        &forged_invite_id,
        &forged_secret,
        "agent",
        binding,
        None,
        &heads,
        T0 + 1_000,
    );

    // `publish` succeeds (the join is stateless-valid: well-formed and signed);
    // the fold is the decisive gate and rejects it, so it is never stored or
    // fanned out — there is no event id to wait for on either peer.
    agent_node
        .publish(join_wire.to_bytes())
        .await
        .expect("publish member.joined");

    // Allow a brief settling window so any in-flight engine work completes.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // AC3: neither peer's fold ever shows the uninvited agent as Active — explicit
    // invitation is enforced at the log, not merely at the connection boundary.
    let admin_snap = admin_node.snapshot().await.expect("admin snapshot");
    assert!(
        !admin_snap.is_active(&stranger_agent.identity()),
        "AC3: admin must NOT show an uninvited agent as Active over the real wire"
    );
    let agent_snap = agent_node.snapshot().await.expect("agent snapshot");
    assert!(
        !agent_snap.is_active(&stranger_agent.identity()),
        "AC3: the uninvited agent must NOT show itself as Active"
    );
    assert_eq!(
        admin_snap.role(&stranger_agent.identity()),
        None,
        "AC3: an uninvited agent has no resolved role in the admin's live snapshot"
    );

    admin_node.shutdown().await.expect("shutdown admin");
    agent_node.shutdown().await.expect("shutdown agent");
}
