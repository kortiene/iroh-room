//! Deterministic, network-free coverage for the **example agent**
//! (`examples/example_agent/main.rs`, issue #39 / IR-0304) — the CI tier of the
//! spec's test plan (§7.1 / D5).
//!
//! The example is an `experimental`-gated binary, so its private `main.rs`
//! helpers cannot be imported here. Following this crate's SDK-test convention
//! (`stable_surface.rs`, `facade_e2e.rs`), these tests reconstruct the
//! example's **pure** steps through the same public façade the example drives,
//! and pin the guarantees the issue's ACs and Test Plan hinge on:
//!
//! * the persisted-identity format's core round-trip — two 32-byte seeds as
//!   hex resolve back to the *same* `identity_id` the admin invited (spec D3);
//! * the `member.joined` → (optional `file.shared`) → `agent.status` sequence
//!   the example authors is well-formed **and** signed by the agent's own
//!   device key while attributed to the agent's identity — this is the "signed
//!   status event" the Test Plan verifies lands in the room tail (spec Step 4/5);
//! * the identity-binding guard that makes an *uninvited* key unable to redeem
//!   a ticket, and the least-privileged `agent` role it joins as (AC3 / spec
//!   Step 4.3 / R2);
//! * that the `--progress 0..=100` bound the example enforces on input is the
//!   same bound the protocol enforces on the wire.
//!
//! The tests above use only the **stable** façade tier (default features), so
//! they always run under `cargo test --workspace`.
//!
//! The `online` module below is the `#[ignore]`-gated loopback tier (spec
//! §7.2): it launches the **built example binary** as a child process against
//! an in-process admin [`Node`](iroh_rooms::experimental::session::Node) over
//! real loopback QUIC and asserts the agent's signed `agent.status` lands in
//! the admin's `room_tail` — the issue's Test Plan, proven end-to-end rather
//! than reconstructed at the library layer. It requires `--features
//! experimental` to even compile (a default-features build cannot name
//! [`Node`](iroh_rooms::experimental::session::Node)) and is gated `#[ignore]`
//! because it spawns a real OS process and a real loopback socket — never run
//! unmarked in CI, mirroring `iroh-rooms-net/tests/join_e2e.rs` and
//! `iroh-rooms-cli/tests/agent_e2e.rs`. Run it with:
//!
//! ```bash
//! cargo test -p iroh-rooms --features experimental --test example_agent_e2e -- --ignored --test-threads=1
//! ```

// admin_identity/admin_device/agent_identity/agent_device are intentionally parallel.
#![allow(clippy::similar_names)]

use iroh_rooms::events::{
    build_agent_status, capability_hash,
    constants::{MAX_STATUS_LABEL_BYTES, MAX_STATUS_MESSAGE_BYTES, SHORT_ID_LEN},
    validate_wire_bytes, Content, EventId, EventType, HashRef, RejectReason, ValidationContext,
};
use iroh_rooms::files::build_file_shared;
use iroh_rooms::identity::{DeviceBinding, SigningKey};
use iroh_rooms::room::{
    build_member_invited, build_member_joined, build_room_created, derive_room_id, RoomInviteTicket,
};

/// A fixed wall-clock stamp so nothing here depends on the real clock (mirrors
/// the golden-vector convention in `core` and the sibling `stable_surface.rs`).
const CREATED_AT: u64 = 1_750_000_000_000;

/// A minted, network-free stand-in for the state the example's `join`
/// subcommand starts from: an admin who created a room and issued a *key-bound*
/// `agent`-role invite, plus the agent's own persisted keypair and a decodable
/// `RoomInviteTicket`. Mirrors the minting in `offline_author_and_validate.rs`
/// and `stable_surface.rs`.
struct Minted {
    admin_identity: SigningKey,
    admin_device: SigningKey,
    agent_identity: SigningKey,
    agent_device: SigningKey,
    ticket: RoomInviteTicket,
    /// A validated causal parent (the on-log `member.invited` id) to hang the
    /// agent's authored `member.joined` off of, exactly as the real heads would.
    parent: EventId,
}

fn mint_agent_ticket() -> Minted {
    let admin_identity = SigningKey::generate();
    let admin_device = SigningKey::generate();
    let agent_identity = SigningKey::generate();
    let agent_device = SigningKey::generate();

    let room_nonce = [0x42; 16];
    let room_id = derive_room_id(&admin_identity.identity_key(), &room_nonce, CREATED_AT);
    let ctx = ValidationContext::for_room(room_id);

    let genesis = build_room_created(
        &admin_identity,
        &admin_device,
        "demo room",
        &room_nonce,
        CREATED_AT,
    );
    let v_genesis = validate_wire_bytes(&genesis.to_bytes(), &ctx).expect("genesis validates");

    // Invite the agent by its *identity key* at the least-privileged `agent`
    // role. The on-log event carries only the capability hash; the ticket
    // carries the secret.
    let invite_id = [0x01; 16];
    let capability_secret = [0x02; 16];
    let cap_hash = capability_hash(&room_id, &invite_id, &capability_secret);
    let invite = build_member_invited(
        &admin_identity,
        &admin_device,
        &room_id,
        &invite_id,
        &cap_hash,
        "agent",
        &agent_identity.identity_key(),
        None,
        None,
        &[v_genesis.event_id],
        CREATED_AT + 1_000,
    );
    let v_invite = validate_wire_bytes(&invite.to_bytes(), &ctx).expect("invite validates");

    let ticket = RoomInviteTicket {
        room_id,
        invite_id,
        capability_secret,
        invitee_key: agent_identity.identity_key(),
        role: "agent".to_owned(),
        expires_at: None,
        inviter_identity: admin_identity.identity_key(),
        discovery: vec![admin_device.device_key()],
    };
    // The ticket the example decodes MUST prove out against the on-log invite.
    assert_eq!(
        ticket.capability_hash(),
        cap_hash,
        "minted ticket must match the on-log member.invited"
    );

    Minted {
        admin_identity,
        admin_device,
        agent_identity,
        agent_device,
        ticket,
        parent: v_invite.event_id,
    }
}

/// Decode one persisted seed line exactly as the example's `decode_seed_line`
/// does: trim, hex-decode, require exactly 32 bytes.
fn decode_seed_line(line: &str) -> [u8; 32] {
    let bytes = hex::decode(line.trim()).expect("seed line is valid hex");
    <[u8; 32]>::try_from(bytes.as_slice()).expect("seed line is exactly 32 bytes")
}

/// Spec D3 — the persisted-identity format is two 32-byte seeds as lowercase
/// hex (one per line). Its whole job is that the id the admin invited is the id
/// the agent redeems with, so a save → load round-trip MUST preserve both the
/// identity key (what the invite is bound to) and the device key (what signs).
/// This is the library-layer half of the identity round-trip; the file I/O and
/// refuse-overwrite guard are exercised by the gated binary tier.
#[test]
fn persisted_identity_seed_format_roundtrips_to_a_stable_id() {
    let identity = SigningKey::generate();
    let device = SigningKey::generate();
    let id_before = identity.identity_key();
    let dev_before = device.device_key();

    // Byte-for-byte the two-line file the example's `save_identity` writes.
    let file_contents = format!(
        "{}\n{}\n",
        hex::encode(*identity.to_seed()),
        hex::encode(*device.to_seed())
    );

    let mut lines = file_contents.lines();
    let identity_seed = decode_seed_line(lines.next().expect("identity seed line present"));
    let device_seed = decode_seed_line(lines.next().expect("device seed line present"));
    let identity_reloaded = SigningKey::from_seed(&identity_seed);
    let device_reloaded = SigningKey::from_seed(&device_seed);

    assert_eq!(
        identity_reloaded.identity_key(),
        id_before,
        "the identity id the admin invites must survive the seed round-trip"
    );
    assert_eq!(
        device_reloaded.device_key(),
        dev_before,
        "the signing device must survive the seed round-trip"
    );
}

/// Spec Step 4/5 + the Test Plan. Reconstruct the exact authoring the example's
/// `join` path performs after connecting — a `member.joined` (with the
/// `example-agent` display name, self-bound via `DeviceBinding::create`) then a
/// default `agent.status` (`state = running_tests`, `progress = 40`). Both must
/// validate statelessly, and the `agent.status` — the event the Test Plan
/// checks for in the room tail — must be **attributed to the agent's identity
/// and signed by the agent's own device key**.
#[test]
fn example_join_then_status_authoring_is_agent_signed_and_valid() {
    let m = mint_agent_ticket();
    let ctx = ValidationContext::for_room(m.ticket.room_id);

    // ── member.joined, exactly as `run_join` builds it ──────────────────────
    let binding = DeviceBinding::create(
        &m.ticket.room_id,
        &m.agent_identity,
        m.agent_device.device_key(),
    );
    let join = build_member_joined(
        &m.agent_identity,
        &m.agent_device,
        &m.ticket.room_id,
        &m.ticket.invite_id,
        &m.ticket.capability_secret,
        &m.ticket.role,
        binding,
        Some("example-agent"),
        &[m.parent],
        CREATED_AT + 2_000,
    );
    let v_join = validate_wire_bytes(&join.to_bytes(), &ctx).expect("member.joined validates");
    assert_eq!(v_join.event.event_type, EventType::MemberJoined);
    assert_eq!(v_join.event.sender_id, m.agent_identity.identity_key());
    assert_eq!(v_join.event.device_id, m.agent_device.device_key());

    // ── agent.status, exactly as the example's default (no --progress) path ──
    let status = build_agent_status(
        &m.agent_identity,
        &m.agent_device,
        &m.ticket.room_id,
        "running_tests",
        Some("cargo test --workspace"),
        &[],
        Some(40),
        &[v_join.event_id],
        CREATED_AT + 3_000,
    );
    let v_status = validate_wire_bytes(&status.to_bytes(), &ctx).expect("agent.status validates");

    assert_eq!(v_status.event.event_type, EventType::AgentStatus);
    // The Test Plan's core: a *signed* status event authored by the agent.
    assert_eq!(
        v_status.event.sender_id,
        m.agent_identity.identity_key(),
        "status must be attributed to the agent identity"
    );
    assert_eq!(
        v_status.event.device_id,
        m.agent_device.device_key(),
        "status must be signed by the agent's own device key"
    );
    match v_status.event.content {
        Content::AgentStatus(s) => {
            assert_eq!(s.status, "running_tests");
            assert_eq!(s.progress_pct, Some(40));
            assert_eq!(s.message.as_deref(), Some("cargo test --workspace"));
            assert_eq!(s.related_artifact_ids, None);
        }
        other => panic!("expected AgentStatus content, got {other:?}"),
    }
}

/// Spec Step 6 (G6, `--artifact`). The example shares one `file.shared` blob and
/// then cites its `file_id` from the subsequent `agent.status`. The blob import
/// itself needs the online store (gated tier), but the *authoring* wiring — a
/// valid `file.shared` and a status that references it — is pure and belongs in
/// the deterministic tier.
#[test]
fn agent_status_can_cite_a_shared_artifact() {
    let m = mint_agent_ticket();
    let ctx = ValidationContext::for_room(m.ticket.room_id);

    let file_id: [u8; SHORT_ID_LEN] = [0x44; SHORT_ID_LEN];
    let shared = build_file_shared(
        &m.agent_identity,
        &m.agent_device,
        &m.ticket.room_id,
        file_id,
        "artifact.txt",
        "application/octet-stream",
        42,
        HashRef::from_bytes([0x33; 32]),
        None,
        &[],
        &[m.parent],
        CREATED_AT + 2_500,
    );
    let v_shared = validate_wire_bytes(&shared.to_bytes(), &ctx).expect("file.shared validates");
    assert_eq!(v_shared.event.event_type, EventType::FileShared);

    let status = build_agent_status(
        &m.agent_identity,
        &m.agent_device,
        &m.ticket.room_id,
        "done",
        Some("published result"),
        &[file_id],
        Some(100),
        &[v_shared.event_id],
        CREATED_AT + 3_000,
    );
    let v_status = validate_wire_bytes(&status.to_bytes(), &ctx).expect("agent.status validates");
    match v_status.event.content {
        Content::AgentStatus(s) => {
            assert_eq!(
                s.related_artifact_ids,
                Some(vec![file_id]),
                "the status must reference the artifact it just shared"
            );
            assert_eq!(s.progress_pct, Some(100));
        }
        other => panic!("expected AgentStatus content, got {other:?}"),
    }
}

/// Spec Step 4.3 / R2 / AC3 — the honest direction of the authorization model.
/// An invite ticket is bound to a *named* invitee identity, so the example's
/// `ensure!(identity.identity_key() == ticket.invitee_key, …)` guard is what
/// stops an uninvited key from redeeming a capability that was never granted to
/// it. Also pins the least-privileged `agent` role the ticket carries.
#[test]
fn uninvited_identity_cannot_satisfy_the_ticket_binding() {
    let m = mint_agent_ticket();

    // The invited key matches — the join proceeds.
    assert_eq!(
        m.agent_identity.identity_key(),
        m.ticket.invitee_key,
        "the persisted identity is the one that was invited"
    );

    // Any other locally generated key does not — the guard fails fast, before
    // any network IO, so a stray keypair can never redeem the ticket.
    let uninvited = SigningKey::generate();
    assert_ne!(
        uninvited.identity_key(),
        m.ticket.invitee_key,
        "a fresh, uninvited identity must not satisfy the ticket binding"
    );

    // The capability the ticket grants is the least-privileged role.
    assert_eq!(m.ticket.role, "agent");
}

/// The `--progress` value the example accepts is `0..=100`; its arg parser
/// rejects anything above 100. This pins that the parser's bound is the same
/// one the protocol enforces on the wire — so a status the example emits can
/// never be rejected for an out-of-range progress it should have caught.
#[test]
fn agent_status_progress_above_100_is_rejected_by_validation() {
    let m = mint_agent_ticket();
    let ctx = ValidationContext::for_room(m.ticket.room_id);

    let build = |progress| {
        build_agent_status(
            &m.agent_identity,
            &m.agent_device,
            &m.ticket.room_id,
            "running_tests",
            None,
            &[],
            Some(progress),
            &[m.parent],
            CREATED_AT + 3_000,
        )
        .to_bytes()
    };

    // At the bound: accepted.
    assert!(validate_wire_bytes(&build(100), &ctx).is_ok());
    // One over: rejected as invalid content — matching the parser's own guard.
    assert!(matches!(
        validate_wire_bytes(&build(101), &ctx),
        Err(RejectReason::InvalidContent)
    ));
}

/// The example's first `join` step is `args.ticket.trim().parse::<RoomInviteTicket>()`.
/// A ticket minted here must survive that copy-paste text round-trip with every
/// field the example reads (`room_id`, `invite_id`, `capability_secret`,
/// `invitee_key`, `role`, `discovery`) intact.
#[test]
fn ticket_token_roundtrips_through_copy_paste_text() {
    let m = mint_agent_ticket();

    let token = m.ticket.to_string();
    // A leading/trailing space is exactly what `.trim()` in the example absorbs.
    let decoded: RoomInviteTicket = format!("  {token}  ")
        .trim()
        .parse()
        .expect("ticket round-trips through its token text");

    assert_eq!(decoded.room_id, m.ticket.room_id);
    assert_eq!(decoded.invite_id, m.ticket.invite_id);
    assert_eq!(decoded.capability_secret, m.ticket.capability_secret);
    assert_eq!(decoded.invitee_key, m.ticket.invitee_key);
    assert_eq!(decoded.role, "agent");
    assert_eq!(decoded.discovery, vec![m.admin_device.device_key()]);
    assert_eq!(decoded.inviter_identity, m.admin_identity.identity_key());
    assert_eq!(decoded.capability_hash(), m.ticket.capability_hash());
}

/// Spec Step 5 / OQ5 — the example resolved OQ5 toward "a single status for
/// maximum minimality": `run_join` passes `args.status`/`args.message`/
/// `args.progress` straight through, and the parser defaults leave `message`
/// and `progress` as `None`. So the *most minimal* invocation the Test Plan
/// runs — `join --ticket <T>` with no `--status`/`--message`/`--progress` — must
/// still author a valid, agent-signed status whose two optional fields are
/// *absent on the wire*, not defaulted to some value. (The sibling
/// `example_join_then_status_authoring_is_agent_signed_and_valid` covers the
/// message+progress path; this pins the bare-defaults path it does not.)
#[test]
fn example_default_minimal_status_is_valid_agent_signed_with_omitted_optionals() {
    let m = mint_agent_ticket();
    let ctx = ValidationContext::for_room(m.ticket.room_id);

    // Byte-for-byte `run_join`'s build call under the parser's bare defaults:
    // status = DEFAULT_STATUS ("running_tests"), message = None, progress = None,
    // and no artifact ⇒ empty `artifact_ids`.
    let wire = build_agent_status(
        &m.agent_identity,
        &m.agent_device,
        &m.ticket.room_id,
        "running_tests",
        None,
        &[],
        None,
        &[m.parent],
        CREATED_AT + 3_000,
    );
    let v = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("default agent.status validates");

    assert_eq!(v.event.event_type, EventType::AgentStatus);
    assert_eq!(
        v.event.sender_id,
        m.agent_identity.identity_key(),
        "even the bare-defaults status is attributed to the agent identity"
    );
    assert_eq!(
        v.event.device_id,
        m.agent_device.device_key(),
        "even the bare-defaults status is signed by the agent's own device key"
    );
    match v.event.content {
        Content::AgentStatus(s) => {
            assert_eq!(s.status, "running_tests");
            assert_eq!(
                s.message, None,
                "omitted --message must not materialize a body"
            );
            assert_eq!(
                s.progress_pct, None,
                "omitted --progress must stay absent, not default to a number"
            );
            assert_eq!(
                s.related_artifact_ids, None,
                "no --artifact ⇒ no references"
            );
        }
        other => panic!("expected AgentStatus content, got {other:?}"),
    }
}

/// The example passes `--status` (default `running_tests`) straight into
/// `build_agent_status` and relies on the pre-publish `validate_wire_bytes`
/// self-check to catch a bad label — it does *not* bound the string itself. So
/// the wire's `state ≤ 64 UTF-8 bytes` rule (and its non-empty rule) is the only
/// thing standing between an over-long/empty `--status` and a silently-emitted
/// bad event: this pins that the self-validate rejects both, so the example
/// fails loudly rather than publishing an invalid status.
#[test]
fn agent_status_label_bounds_match_the_wire() {
    let m = mint_agent_ticket();
    let ctx = ValidationContext::for_room(m.ticket.room_id);

    let build = |status: &str| {
        build_agent_status(
            &m.agent_identity,
            &m.agent_device,
            &m.ticket.room_id,
            status,
            None,
            &[],
            None,
            &[m.parent],
            CREATED_AT + 3_000,
        )
        .to_bytes()
    };

    // At the 64-byte bound: accepted.
    assert!(validate_wire_bytes(&build(&"a".repeat(MAX_STATUS_LABEL_BYTES)), &ctx).is_ok());
    // One byte over: rejected — the example's self-validate would surface this.
    assert!(matches!(
        validate_wire_bytes(&build(&"a".repeat(MAX_STATUS_LABEL_BYTES + 1)), &ctx),
        Err(RejectReason::InvalidContent)
    ));
    // An empty `--status` is equally invalid — the label must carry meaning.
    assert!(matches!(
        validate_wire_bytes(&build(""), &ctx),
        Err(RejectReason::InvalidContent)
    ));
}

/// Companion to the label bound: the optional `--message` the example forwards
/// verbatim is bounded at 4096 UTF-8 bytes on the wire. Pins that a message at
/// the bound is accepted and one byte over is rejected, so a caller-supplied
/// `--message` can never smuggle an over-long note past the self-validate.
#[test]
fn agent_status_message_bound_matches_the_wire() {
    let m = mint_agent_ticket();
    let ctx = ValidationContext::for_room(m.ticket.room_id);

    let build = |message: &str| {
        build_agent_status(
            &m.agent_identity,
            &m.agent_device,
            &m.ticket.room_id,
            "running_tests",
            Some(message),
            &[],
            None,
            &[m.parent],
            CREATED_AT + 3_000,
        )
        .to_bytes()
    };

    // At the 4096-byte bound: accepted.
    assert!(validate_wire_bytes(&build(&"m".repeat(MAX_STATUS_MESSAGE_BYTES)), &ctx).is_ok());
    // One byte over: rejected.
    assert!(matches!(
        validate_wire_bytes(&build(&"m".repeat(MAX_STATUS_MESSAGE_BYTES + 1)), &ctx),
        Err(RejectReason::InvalidContent)
    ));
}

// ═══════════════════════════════════════════════════════════════════════════
// Online tier (spec §7.2, D5) — `#[ignore]`-gated; launches the *built*
// `example_agent` binary as a child process against an in-process admin
// [`Node`] over real loopback QUIC, and asserts the issue's Test Plan
// directly: "run the example in a local demo room and verify [a] signed
// status event appears in room tail." Requires `--features experimental`.
//
// A third test below additionally exercises the optional `--artifact` path
// (issue G6) through the *built binary*: the pure-authoring wiring is already
// pinned by `agent_status_can_cite_a_shared_artifact` above, but that test
// never touches a real `BlobStore`; this one proves the actual import +
// publish + admin-side observation round-trips end-to-end.
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(feature = "experimental")]
mod online {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use iroh_rooms::events::{
        capability_hash, validate_wire_bytes, Content, EventType, ValidationContext,
    };
    use iroh_rooms::experimental::session::{
        AllowlistAdmission, EndpointAddr, JoinBootstrapAdmission, NetConfig, NetMode, Node,
        SecretKey, TracingAudit, DEFAULT_TICK,
    };
    use iroh_rooms::experimental::store::EventStore;
    use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
    use iroh_rooms::identity::SigningKey;
    use iroh_rooms::room::{
        build_member_invited, build_room_created, derive_room_id, RoomId, RoomInviteTicket,
    };

    /// Loopback-wait budget for network convergence (mirrors `join_e2e.rs`).
    const WAIT: Duration = Duration::from_secs(15);
    /// Budget for the child `example_agent join` process. Generous — it
    /// covers the network-side waits the example itself bounds internally
    /// (connect / membership-bootstrap / active-confirm, each up to its own
    /// `JOIN_TIMEOUT`) plus `cargo run`'s freshness-check overhead; the
    /// expensive first build is already paid for by
    /// [`ensure_example_agent_built`]. Never runs in CI, so this being
    /// generous is not a CI-latency concern (spec OQ4).
    const CHILD_TIMEOUT: Duration = Duration::from_mins(2);
    /// Timeout safety bound for the negative case. The proof that the
    /// identity-binding guard fires pre-IO is the *error message* assertion
    /// below (the example's `ensure!` text, not a `JOIN_TIMEOUT` connect-
    /// timeout message) — this bound only guards against a genuine hang.
    /// [`ensure_example_agent_built`] pays the (untimed) build cost before
    /// either test's timed run, so by the time this bound applies, `cargo
    /// run -q` is just re-verifying freshness and executing an
    /// already-built artifact — comfortably fast, still well under
    /// [`CHILD_TIMEOUT`].
    const FAST_FAIL_TIMEOUT: Duration = Duration::from_secs(10);

    const CREATED_AT: u64 = 1_750_000_000_000;

    /// Everything an admin needs to pre-seed its store, plus the ticket and
    /// keys the child `example_agent` process needs to redeem it.
    struct OnlineFixture {
        room_id: RoomId,
        /// Wire bytes to seed the admin's engine: `[genesis, invite]`.
        log: Vec<Vec<u8>>,
        ticket: RoomInviteTicket,
        admin_device: SigningKey,
        agent_identity: SigningKey,
        agent_device: SigningKey,
    }

    /// Mint an admin room with a single `agent`-role invite bound to a fresh
    /// agent identity, and the matching [`RoomInviteTicket`] — the online
    /// counterpart of the top-level `mint_agent_ticket`, retaining the raw
    /// wire bytes needed to pre-seed a real [`EventStore`].
    fn mint_online_fixture() -> OnlineFixture {
        let admin_identity = SigningKey::generate();
        let admin_device = SigningKey::generate();
        let agent_identity = SigningKey::generate();
        let agent_device = SigningKey::generate();

        let room_nonce = [0x77; 16];
        let room_id = derive_room_id(&admin_identity.identity_key(), &room_nonce, CREATED_AT);
        let ctx = ValidationContext::for_room(room_id);

        let genesis = build_room_created(
            &admin_identity,
            &admin_device,
            "example agent e2e room",
            &room_nonce,
            CREATED_AT,
        );
        let genesis_bytes = genesis.to_bytes();
        let v_genesis = validate_wire_bytes(&genesis_bytes, &ctx).expect("genesis validates");

        let invite_id = [0x09; 16];
        let capability_secret = [0x0a; 16];
        let cap_hash = capability_hash(&room_id, &invite_id, &capability_secret);
        let invite = build_member_invited(
            &admin_identity,
            &admin_device,
            &room_id,
            &invite_id,
            &cap_hash,
            "agent",
            &agent_identity.identity_key(),
            None,
            None,
            &[v_genesis.event_id],
            CREATED_AT + 1_000,
        );
        let invite_bytes = invite.to_bytes();
        validate_wire_bytes(&invite_bytes, &ctx).expect("invite validates");

        let ticket = RoomInviteTicket {
            room_id,
            invite_id,
            capability_secret,
            invitee_key: agent_identity.identity_key(),
            role: "agent".to_owned(),
            expires_at: None,
            inviter_identity: admin_identity.identity_key(),
            discovery: vec![admin_device.device_key()],
        };

        OnlineFixture {
            room_id,
            log: vec![genesis_bytes, invite_bytes],
            ticket,
            admin_device,
            agent_identity,
            agent_device,
        }
    }

    /// Spawn an admin [`Node`] pre-seeded with `fx.log`, using
    /// [`JoinBootstrapAdmission`] so the agent's not-yet-known device is
    /// admitted provisionally — exactly `join_e2e.rs`'s `spawn_admin_node`.
    async fn spawn_admin_node(fx: &OnlineFixture) -> Node {
        let store = EventStore::open_in_memory().expect("in-memory admin store");
        let mut engine =
            SyncEngine::open(store, fx.room_id, SyncConfig::default()).expect("admin engine");
        for ev in &fx.log {
            engine.publish(ev).expect("seed admin event");
        }
        let admission = JoinBootstrapAdmission::new(AllowlistAdmission::new(), true);
        let cfg = NetConfig {
            mode: NetMode::Loopback,
            ..NetConfig::default()
        };
        Node::spawn(
            fx.admin_device.iroh_secret(),
            Arc::new(admission),
            Arc::new(TracingAudit),
            engine,
            cfg,
            DEFAULT_TICK,
        )
        .await
        .expect("spawn admin node")
    }

    /// Render an [`EndpointAddr`] as the `--peer` wire form
    /// `<ENDPOINT_ID>[@<ip:port>,...]` the example's own `parse_peer_addr`
    /// accepts — byte-for-byte the CLI's `render_endpoint_addr`
    /// (`iroh-rooms-cli/src/message.rs`), reimplemented here since it is a
    /// crate-private helper.
    fn render_endpoint_addr(addr: &EndpointAddr) -> String {
        let socks: Vec<String> = addr.ip_addrs().map(ToString::to_string).collect();
        if socks.is_empty() {
            addr.id.to_string()
        } else {
            format!("{}@{}", addr.id, socks.join(","))
        }
    }

    /// Write the two-line hex-seed identity file the example's
    /// `save_identity`/`load_identity` read (spec D3).
    fn write_identity_file(path: &std::path::Path, identity: &SigningKey, device: &SigningKey) {
        let contents = format!(
            "{}\n{}\n",
            hex::encode(*identity.to_seed()),
            hex::encode(*device.to_seed())
        );
        std::fs::write(path, contents).expect("write identity file");
    }

    /// Small extension trait so [`OnlineFixture`]'s `admin_device` can produce
    /// the [`SecretKey`] transport identity the same way the example's own
    /// `join` subcommand derives it (`SecretKey::from_bytes(&device.to_seed())`).
    trait IrohSecret {
        fn iroh_secret(&self) -> SecretKey;
    }

    impl IrohSecret for SigningKey {
        fn iroh_secret(&self) -> SecretKey {
            SecretKey::from_bytes(&self.to_seed())
        }
    }

    /// Build the `example_agent` binary once per test-binary process, with no
    /// artificial timeout (mirrors the untimed `cargo build` every other
    /// binary-launching e2e suite in this workspace relies on via
    /// `assert_cmd::cargo::cargo_bin`). Doing this up front — instead of
    /// letting the first [`run_example_agent`] call pay for it — keeps that
    /// function's own timeout a tight bound on the *example's* behavior
    /// rather than on cargo's build/freshness-check overhead. `Once` also
    /// makes this safe if a future revision drops the `--test-threads=1`
    /// convention.
    fn ensure_example_agent_built() {
        static BUILD: std::sync::Once = std::sync::Once::new();
        BUILD.call_once(|| {
            let status = Command::new(env!("CARGO"))
                .args([
                    "build",
                    "-q",
                    "-p",
                    "iroh-rooms",
                    "--features",
                    "experimental",
                    "--example",
                    "example_agent",
                ])
                .status()
                .expect("run cargo build for example_agent");
            assert!(
                status.success(),
                "cargo build for example_agent must succeed"
            );
        });
    }

    /// The captured result of running the built `example_agent` binary once.
    struct ExampleRun {
        success: bool,
        stdout: String,
        stderr: String,
    }

    fn drain(pipe: impl Read + Send + 'static, into: Arc<Mutex<String>>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines() {
                let Ok(line) = line else { break };
                let mut buf = into.lock().expect("capture buffer not poisoned");
                buf.push_str(&line);
                buf.push('\n');
            }
        })
    }

    /// Run `cargo run -q -p iroh-rooms --features experimental --example
    /// example_agent -- <args>` as a child process and wait (bounded) for it
    /// to exit, capturing stdout/stderr. Never hangs: kills + panics with the
    /// captured output on timeout (spec R4).
    fn run_example_agent(args: &[&str], timeout: Duration) -> ExampleRun {
        let mut child = Command::new(env!("CARGO"))
            .args([
                "run",
                "-q",
                "-p",
                "iroh-rooms",
                "--features",
                "experimental",
                "--example",
                "example_agent",
                "--",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn example_agent child process");

        let stdout_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let out = child.stdout.take().expect("child stdout is piped");
        let err = child.stderr.take().expect("child stderr is piped");
        let out_reader = drain(out, Arc::clone(&stdout_buf));
        let err_reader = drain(err, Arc::clone(&stderr_buf));

        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll child status") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                panic!(
                    "example_agent {args:?} did not exit within {timeout:?}\n\
                     --- stdout ---\n{}\n--- stderr ---\n{}",
                    stdout_buf.lock().expect("capture buffer not poisoned"),
                    stderr_buf.lock().expect("capture buffer not poisoned"),
                );
            }
            thread::sleep(Duration::from_millis(50));
        };
        let _ = out_reader.join();
        let _ = err_reader.join();

        let stdout = stdout_buf
            .lock()
            .expect("capture buffer not poisoned")
            .clone();
        let stderr = stderr_buf
            .lock()
            .expect("capture buffer not poisoned")
            .clone();
        ExampleRun {
            success: status.success(),
            stdout,
            stderr,
        }
    }

    /// The issue's Test Plan, verbatim: run the example agent in a local demo
    /// room and verify a signed status event appears in the room tail. An
    /// in-process admin `Node` stands in for `iroh-rooms room tail
    /// --accept-joins --loopback`; the child process is the *actual built
    /// example binary*, driven exactly as the README's clean-checkout flow
    /// documents (`join --ticket … --peer … --status … --message … --progress
    /// … --loopback`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "spawns a real child process + loopback socket; run with --ignored --test-threads=1"]
    async fn example_agent_status_lands_signed_in_admin_room_tail() {
        ensure_example_agent_built();
        let fx = mint_online_fixture();
        let admin_node = spawn_admin_node(&fx).await;

        let tmp = tempfile::TempDir::new().expect("temp dir for identity file");
        let identity_path = tmp.path().join("agent.identity");
        write_identity_file(&identity_path, &fx.agent_identity, &fx.agent_device);

        let admin_addr = admin_node.endpoint_addr().expect("admin endpoint_addr");
        let peer_arg = render_endpoint_addr(&admin_addr);
        let ticket_arg = fx.ticket.to_string();
        let identity_path_str = identity_path.to_str().expect("utf8 temp path").to_owned();

        let run = run_example_agent(
            &[
                "join",
                "--identity-file",
                &identity_path_str,
                "--ticket",
                &ticket_arg,
                "--peer",
                &peer_arg,
                "--status",
                "running_tests",
                "--message",
                "example agent e2e",
                "--progress",
                "40",
                "--loopback",
            ],
            CHILD_TIMEOUT,
        );
        assert!(
            run.success,
            "example_agent join must exit 0; stdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        );
        assert!(
            run.stdout.contains("joined:"),
            "example_agent must report the join; stdout:\n{}",
            run.stdout
        );

        // Poll the admin's room_tail — the issue's oracle — until the signed
        // status appears, or fail with a clear timeout (never hang).
        let ctx = ValidationContext::for_room(fx.room_id);
        let deadline = Instant::now() + WAIT;
        let found = loop {
            let tail = admin_node.room_tail(100).await.expect("admin room_tail");
            let status_row = tail
                .iter()
                .find(|ev| ev.event_type == EventType::AgentStatus);
            if let Some(stored) = status_row {
                let validated = validate_wire_bytes(&stored.wire.to_bytes(), &ctx)
                    .expect("stored agent.status re-validates");
                break validated;
            }
            assert!(
                Instant::now() < deadline,
                "the admin's room_tail never showed an agent.status within {WAIT:?}; \
                 stdout:\n{}\nstderr:\n{}",
                run.stdout,
                run.stderr
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        // The Test Plan's core: a *signed* status event, attributed to the
        // agent's own identity and device key — not the admin's.
        assert_eq!(
            found.event.sender_id,
            fx.agent_identity.identity_key(),
            "the room-tail status must be attributed to the agent identity"
        );
        assert_eq!(
            found.event.device_id,
            fx.agent_device.device_key(),
            "the room-tail status must be signed by the agent's own device key"
        );
        match found.event.content {
            Content::AgentStatus(s) => {
                assert_eq!(s.status, "running_tests");
                assert_eq!(s.progress_pct, Some(40));
                assert_eq!(s.message.as_deref(), Some("example agent e2e"));
            }
            other => panic!("expected AgentStatus content, got {other:?}"),
        }

        admin_node.shutdown().await.expect("shutdown admin node");
    }

    /// Issue G6 / spec Step 6 — the optional `--artifact <PATH>` path, run
    /// through the *built binary* rather than reconstructed at the library
    /// layer (that pure-authoring half is already pinned by
    /// `agent_status_can_cite_a_shared_artifact` above). This proves the
    /// example's actual `BlobStore` import produces a `file.shared` whose
    /// `blob_hash` matches an *independently computed* BLAKE3 hash of the
    /// fixture bytes (not just the example's own claimed hash), that the
    /// admin observes it in `room_tail`, and that the subsequent
    /// `agent.status` cites the same `file_id` — the "do work, publish a
    /// result" loop the issue's scope calls out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "spawns a real child process + loopback socket; run with --ignored --test-threads=1"]
    async fn example_agent_shares_artifact_and_status_cites_it() {
        ensure_example_agent_built();
        let fx = mint_online_fixture();
        let admin_node = spawn_admin_node(&fx).await;

        let tmp = tempfile::TempDir::new().expect("temp dir for identity + artifact");
        let identity_path = tmp.path().join("agent.identity");
        write_identity_file(&identity_path, &fx.agent_identity, &fx.agent_device);

        let artifact_bytes = b"hello from the example agent's --artifact fixture\n";
        let artifact_path = tmp.path().join("result.txt");
        std::fs::write(&artifact_path, artifact_bytes).expect("write artifact fixture");
        let expected_hash = blake3::hash(artifact_bytes);

        let admin_addr = admin_node.endpoint_addr().expect("admin endpoint_addr");
        let peer_arg = render_endpoint_addr(&admin_addr);
        let ticket_arg = fx.ticket.to_string();
        let identity_path_str = identity_path.to_str().expect("utf8 temp path").to_owned();
        let artifact_path_str = artifact_path.to_str().expect("utf8 temp path").to_owned();

        let run = run_example_agent(
            &[
                "join",
                "--identity-file",
                &identity_path_str,
                "--ticket",
                &ticket_arg,
                "--peer",
                &peer_arg,
                "--artifact",
                &artifact_path_str,
                "--status",
                "done",
                "--progress",
                "100",
                "--loopback",
            ],
            CHILD_TIMEOUT,
        );
        assert!(
            run.success,
            "example_agent join --artifact must exit 0; stdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        );
        assert!(
            run.stdout.contains("shared artifact:"),
            "example_agent must report the artifact share; stdout:\n{}",
            run.stdout
        );

        // Poll the admin's room_tail until both the file.shared and the
        // agent.status that cites it appear.
        let ctx = ValidationContext::for_room(fx.room_id);
        let deadline = Instant::now() + WAIT;
        let (shared_event, status_event) = loop {
            let tail = admin_node.room_tail(100).await.expect("admin room_tail");
            let shared = tail
                .iter()
                .find(|ev| ev.event_type == EventType::FileShared);
            let status = tail
                .iter()
                .find(|ev| ev.event_type == EventType::AgentStatus);
            if let (Some(shared), Some(status)) = (shared, status) {
                let v_shared = validate_wire_bytes(&shared.wire.to_bytes(), &ctx)
                    .expect("stored file.shared re-validates");
                let v_status = validate_wire_bytes(&status.wire.to_bytes(), &ctx)
                    .expect("stored agent.status re-validates");
                break (v_shared, v_status);
            }
            assert!(
                Instant::now() < deadline,
                "the admin's room_tail never showed both a file.shared and an agent.status \
                 within {WAIT:?}; stdout:\n{}\nstderr:\n{}",
                run.stdout,
                run.stderr
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let file_id = match shared_event.event.content {
            Content::FileShared(f) => {
                assert_eq!(f.name, "result.txt");
                assert_eq!(f.size_bytes, artifact_bytes.len() as u64);
                assert_eq!(
                    f.blob_hash.as_bytes(),
                    expected_hash.as_bytes(),
                    "the published blob_hash must match an independently computed BLAKE3 hash \
                     of the fixture bytes, not just whatever the example claims"
                );
                f.file_id
            }
            other => panic!("expected FileShared content, got {other:?}"),
        };
        match status_event.event.content {
            Content::AgentStatus(s) => {
                assert_eq!(
                    s.related_artifact_ids,
                    Some(vec![file_id]),
                    "the status must cite the file_id the example just shared"
                );
                assert_eq!(s.progress_pct, Some(100));
            }
            other => panic!("expected AgentStatus content, got {other:?}"),
        }

        admin_node.shutdown().await.expect("shutdown admin node");
    }

    /// Spec §7.2 negative check / R2 / AC3's honest direction: an identity
    /// that was never invited cannot redeem a ticket bound to someone else,
    /// and the example's `ensure!` guard catches it **before any network
    /// IO** — proven here by the exact `ensure!` wording surfacing on stderr
    /// (not the distinct `JOIN_TIMEOUT` connect-timeout message a network
    /// failure would produce) and by the admin's `room_tail` staying exactly
    /// at the two pre-seeded events (genesis + invite): no `member.joined`
    /// and no connection was ever observed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "spawns a real child process + loopback socket; run with --ignored --test-threads=1"]
    async fn example_agent_rejects_ticket_not_bound_to_its_identity() {
        ensure_example_agent_built();
        let fx = mint_online_fixture();
        let admin_node = spawn_admin_node(&fx).await;

        // A fresh identity the ticket was never bound to.
        let uninvited_identity = SigningKey::generate();
        let uninvited_device = SigningKey::generate();
        let tmp = tempfile::TempDir::new().expect("temp dir for identity file");
        let identity_path = tmp.path().join("uninvited.identity");
        write_identity_file(&identity_path, &uninvited_identity, &uninvited_device);

        let admin_addr = admin_node.endpoint_addr().expect("admin endpoint_addr");
        let peer_arg = render_endpoint_addr(&admin_addr);
        let ticket_arg = fx.ticket.to_string();
        let identity_path_str = identity_path.to_str().expect("utf8 temp path").to_owned();

        let run = run_example_agent(
            &[
                "join",
                "--identity-file",
                &identity_path_str,
                "--ticket",
                &ticket_arg,
                "--peer",
                &peer_arg,
                "--loopback",
            ],
            FAST_FAIL_TIMEOUT,
        );
        assert!(
            !run.success,
            "an uninvited identity must not be able to join; stdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        );
        assert!(
            run.stderr.contains("was not the one invited"),
            "the failure must be the identity-binding guard, not some other error; stderr:\n{}",
            run.stderr
        );

        // No network IO occurred: the admin never sees a member.joined, and
        // its store still holds exactly the two pre-seeded events.
        let tail = admin_node.room_tail(100).await.expect("admin room_tail");
        assert_eq!(
            tail.len(),
            fx.log.len(),
            "the admin's store must be unchanged — the guard fires pre-IO, before any dial"
        );
        assert!(
            tail.iter()
                .all(|ev| ev.event_type != EventType::MemberJoined),
            "no member.joined may reach the admin from a ticket-binding rejection"
        );

        admin_node.shutdown().await.expect("shutdown admin node");
    }
}
