//! Room join by ticket: the orchestration behind
//! `iroh-rooms room join <TICKET> [--peer …] [--display-name …] [--timeout …]`
//! (spec IR-0104).
//!
//! `room join` is the third **online** CLI command (after `room send` / `room
//! tail`) and the joiner half of the invite handshake. It lets an **invited** peer
//! redeem a `roomtkt1…` ticket and become an `Active` member by authoring a valid
//! `member.joined` both peers converge on. It stays a thin orchestrator over landed
//! primitives — the ticket codec, the pure [`build_member_joined`] assembler, the
//! membership fold's `gate_join`, and the bounded recent-sync engine — plus the one
//! genuinely new seam: the join-bootstrap admission overlay in
//! [`iroh_rooms_net`](iroh_rooms_net::JoinBootstrapAdmission) that lets a
//! not-yet-`Active` invitee pull the membership sub-DAG and push its join.
//!
//! The flow (spec D6):
//!
//! 1. **Decode** the ticket (fail-closed) and **pre-check** the key binding (local
//!    identity must equal `ticket.invitee_key`) — both before any network/store IO.
//! 2. Bring up an ephemeral [`Node`], dial the admin (the ticket's discovery hint /
//!    `--peer`), and **pull** the never-windowed membership sub-DAG via the engine's
//!    existing `WantMembership` handshake (we wait until we resolve as `Invited`).
//! 3. **Build** a `member.joined` whose `device_binding` attests `(invitee_key,
//!    device)` under `room_id` and whose `capability_secret` proves the invite.
//! 4. **Self-validate + fold-check** it locally (stateless §6 pipeline + `gate_join`)
//!    so a bad secret / expiry / role fails with a friendly, deterministic message
//!    instead of a doomed push, then **publish** it so the admin folds + persists it.
//!
//! Authorization is **not** decided here: every peer re-runs `gate_join` on the
//! join's fixed causal ancestors. The CLI's identity pre-check is a friendly
//! fast-fail; the on-log gate is the convergent authority (spec §9).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms::experimental::session::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::build_member_joined;
use iroh_rooms_core::event::constants::SHORT_ID_LEN;
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::RejectReason;
use iroh_rooms_core::membership::Ingest;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_core::ticket::{RoomInviteTicket, TicketError};
use iroh_rooms_net::{AllowlistAdmission, NetConfig, Node, DEFAULT_TICK};
use zeroize::Zeroizing;

use crate::error::{CodedResultExt, ErrorCode};
use crate::message::{
    endpoint_id_of, fold_room, net_mode, parse_peers, render_endpoint_addr, select_heads, DB_FILE,
};
use crate::{audit, clock, identity};

/// Default time budget for both the membership-pull and the post-publish
/// confirmation waits (spec §5). `<int>{ms|s|m}`.
pub const DEFAULT_JOIN_TIMEOUT: &str = "10s";

/// Poll interval while waiting for the membership pull / local `Active` transition.
const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Grace after publishing so the join frame flushes to the admin before we tear the
/// ephemeral node down (mirrors `message::send`'s flush grace; spec D6 step 12 /
/// OQ7 best-effort admin confirmation).
const PUBLISH_FLUSH_GRACE: Duration = Duration::from_millis(500);

/// The result of a successful `room join`, for the caller to present.
pub struct JoinSummary {
    /// The authored `member.joined` event id.
    pub event_id: EventId,
    /// The room joined.
    pub room_id: RoomId,
    /// The room name, resolved from the pulled genesis when available.
    pub room_name: Option<String>,
    /// The joined role (`member` | `agent`), copied from the invite.
    pub role: String,
    /// Active-member count after the join (a sanity hook).
    pub active_members: usize,
}

/// Redeem `ticket_str` and join its room: decode + key-binding pre-check, bootstrap
/// the membership sub-DAG from the admin, build + self-validate + fold-check the
/// `member.joined`, and publish it.
///
/// # Errors
/// Fails — touching no network or store on the pre-IO paths — if the ticket is
/// malformed, if the local identity is not the invite's bound `invitee_key`, if no
/// local identity exists, if `--peer` is invalid, if the ticket carries no admin
/// discovery hint, if the membership pull does not complete within `timeout` (a join
/// is online: this is a failure, not a silent local success), or if the local
/// fold-check rejects the join (bad secret / expiry / role) — the deterministic
/// verdict every peer reaches. A failure to confirm the admin observed the join
/// within `timeout` is reported, not failed (the join is stored locally).
#[allow(clippy::too_many_arguments)] // one linear orchestration; each arg is a distinct CLI input
pub async fn join(
    home: &Path,
    ticket_str: &str,
    peers: &[String],
    display_name: Option<&str>,
    timeout: Duration,
    loopback: bool,
) -> Result<JoinSummary> {
    // ---- Pre-IO: decode the ticket and pre-check the key binding (D3). ----
    // AC3: the reason is the ticket's own redacted `Display` (never the token/
    // secret), tagged with the matching `ticket_*` code.
    let ticket: RoomInviteTicket = ticket_str
        .trim()
        .parse::<RoomInviteTicket>()
        .with_coded(|e: &TicketError| ErrorCode::Ticket(e.clone()))?;

    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    if self_id != ticket.invitee_key {
        // The wrong-identity acceptance criterion as a friendly, no-network failure;
        // the on-log `gate_join` (key binding) remains the convergent authority.
        crate::bail_coded!(
            ErrorCode::WrongIdentity,
            "this invite ticket is bound to a different identity ({}); your identity is {}",
            ticket.invitee_key,
            self_id
        );
    }

    let peer_addrs = parse_peers(peers)?;
    let dial_set = build_bootstrap_dial_set(&ticket, &peer_addrs)?;
    if dial_set.is_empty() {
        crate::bail_coded!(
            ErrorCode::NoDiscoveryHint,
            "the invite ticket carries no admin discovery hint; cannot reach the room admin to \
             bootstrap the join"
        );
    }

    // Hold the capability secret in a Zeroizing buffer from decode until it is placed
    // in the join content (D2). It legitimately lands on the log inside the join (the
    // protocol), but the CLI never prints it.
    let capability_secret = Zeroizing::new(ticket.capability_secret);
    let via_invite_id = ticket.invite_id;
    let room_id = ticket.room_id;
    let role = ticket.role.clone();

    // ---- Bring up the joiner node with a ticket-derived admission gate. ----
    // We only ever talk to the admin: bind every discovery device to the inviter's
    // identity and mark it Active so our gate admits the admin's dial-back / replies
    // (D6 step 3). The admin admits *us* provisionally (its `JoinBootstrapAdmission`).
    let mut admission = AllowlistAdmission::new();
    for dev in &ticket.discovery {
        admission = admission.bind_device(endpoint_id_of(*dev)?, ticket.inviter_identity);
    }
    admission = admission.set_active(ticket.inviter_identity);

    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let engine = SyncEngine::open(store, room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;

    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    let node = Node::spawn(
        secret_key,
        Arc::new(admission),
        Arc::new(audit::StderrAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .context("could not bring up the network node")?;

    if let Ok(addr) = node.endpoint_addr() {
        println!("listening: {}", render_endpoint_addr(&addr));
    }
    for addr in dial_set {
        node.connect_to(addr);
    }

    // Run the bootstrap + publish, then always tear the node down (so the pump and
    // router stop even on an error path).
    let outcome = bootstrap_and_join(
        &node,
        &db_path,
        home,
        &room_id,
        &self_id,
        &secret,
        &capability_secret,
        &via_invite_id,
        &role,
        display_name,
        timeout,
    )
    .await;

    node.shutdown()
        .await
        .context("could not shut down the network node")?;
    outcome
}

/// The post-bring-up half of [`join`]: wait for the membership pull, build the join,
/// fold-check it, publish it, and confirm local `Active`. Split out so [`join`] can
/// guarantee `Node::shutdown` on every path.
#[allow(clippy::too_many_arguments)]
async fn bootstrap_and_join(
    node: &Node,
    db_path: &Path,
    home: &Path,
    room_id: &RoomId,
    self_id: &IdentityKey,
    secret: &identity::SecretKeys,
    capability_secret: &[u8; SHORT_ID_LEN],
    via_invite_id: &[u8; SHORT_ID_LEN],
    role: &str,
    display_name: Option<&str>,
    timeout: Duration,
) -> Result<JoinSummary> {
    // ---- Wait for the membership sub-DAG: genesis + our naming invite, persisted,
    // so we resolve as `Invited` (D6 step 5). A read-only second handle on the same
    // SQLite file sees the engine's committed pulls (WAL). ----
    let store = EventStore::open(db_path)
        .with_context(|| format!("could not reopen event store at {}", db_path.display()))?;
    wait_for_invited(&store, home, room_id, self_id, timeout).await?;

    // ---- Build the join from the pulled heads (D6 steps 6–8). ----
    let heads = select_heads(&store, room_id)?;
    let created_at = clock::now_ms();
    let binding = DeviceBinding::create(room_id, &secret.identity, secret.device.device_key());
    let wire = build_member_joined(
        &secret.identity,
        &secret.device,
        room_id,
        via_invite_id,
        capability_secret,
        role,
        binding,
        display_name,
        &heads,
        created_at,
    );
    let wire_bytes = wire.to_bytes();

    // ---- Stateless self-validate (internal-bug guard, D6 step 9). ----
    let ctx = ValidationContext::for_room(*room_id);
    let validated = validate_wire_bytes(&wire_bytes, &ctx)
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built member.joined failed validation ({})",
                reason.code()
            )
        })
        .coded(ErrorCode::Internal)?;
    let event_id = validated.event_id;

    // ---- Local fold-check (D6 step 10): a clean, deterministic error for a bad
    // secret / expiry / role before a doomed push. The same verdict the admin and
    // every peer reach. ----
    let (mut membership, _snapshot) = fold_room(&store, home, room_id)?;
    match membership.ingest(validated) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => {
            let message = join_reject_message(reason.clone());
            return Err(crate::error::CliError::new(ErrorCode::Reject(reason), message).into());
        }
        Ingest::Buffered { .. } => bail!(
            "could not place the join in the room history (its causal ancestors are incomplete); \
             retry once the admin has finished sharing the membership history"
        ),
    }

    // ---- Publish: the engine ingests (persisting it) and fans it out to the admin,
    // which folds + persists it too (D6 step 11). ----
    node.publish(wire_bytes)
        .await
        .context("could not publish the join to the room")?;

    // ---- Confirm local Active, then a brief grace so the admin ingests it before we
    // tear down (D6 step 12; admin confirmation is best-effort, OQ7). ----
    wait_for_active(node, self_id, timeout).await?;
    tokio::time::sleep(PUBLISH_FLUSH_GRACE).await;

    // ---- Summary from the freshest snapshot (the publish already persisted locally). ----
    let snapshot = node
        .snapshot()
        .await
        .context("could not read the post-join membership snapshot")?;
    let active_members = snapshot.active_members().count();
    let room_name = room_name_from_store(&store, room_id);

    Ok(JoinSummary {
        event_id,
        room_id: *room_id,
        room_name,
        role: role.to_owned(),
        active_members,
    })
}

/// Print a [`JoinSummary`] as labeled, script-friendly lines (spec D10). The
/// `capability_secret` never appears here.
pub fn print_join(summary: &JoinSummary) {
    println!("joined: {}", summary.event_id);
    println!("room: {}", summary.room_id);
    if let Some(name) = &summary.room_name {
        println!("name: {name}");
    }
    println!("role: {}", summary.role);
    println!("members: {} active", summary.active_members);
    println!(
        "next: run `iroh-rooms room members {room}` or `iroh-rooms room tail {room}`",
        room = summary.room_id
    );
}

// ---------------------------------------------------------------------------
// Bootstrap helpers
// ---------------------------------------------------------------------------

/// The bootstrap dial set: each of the ticket's discovery devices (MVP = the single
/// admin device), addressed by a matching `--peer` when one is supplied (the
/// deterministic LAN/loopback dial) else a bare [`EndpointAddr`] resolved through
/// discovery (D4). Mirrors `message::build_dial_set`'s id-matching.
fn build_bootstrap_dial_set(
    ticket: &RoomInviteTicket,
    peer_addrs: &[EndpointAddr],
) -> Result<Vec<EndpointAddr>> {
    let by_id: BTreeMap<EndpointId, EndpointAddr> =
        peer_addrs.iter().map(|a| (a.id, a.clone())).collect();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for dev in &ticket.discovery {
        let id = endpoint_id_of(*dev)?;
        if !seen.insert(id) {
            continue;
        }
        out.push(
            by_id
                .get(&id)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(id)),
        );
    }
    Ok(out)
}

/// Wait (≤ `timeout`) until the local store resolves `self_id` to a known membership
/// status — i.e. the genesis + our naming `member.invited` have been pulled and
/// persisted (`status(self).is_some()`). Re-folds the store on each poll, so it sees
/// the engine's freshly-committed pulls.
async fn wait_for_invited(
    store: &EventStore,
    home: &Path,
    room_id: &RoomId,
    self_id: &IdentityKey,
    timeout: Duration,
) -> Result<()> {
    let polled = tokio::time::timeout(timeout, async {
        loop {
            // `fold_room` errs while the store is still empty (no room yet); ignore
            // and keep polling until the pull lands.
            if let Ok((_, snapshot)) = fold_room(store, home, room_id) {
                if snapshot.status(self_id).is_some() {
                    return;
                }
            }
            tokio::time::sleep(JOIN_POLL_INTERVAL).await;
        }
    })
    .await;
    // Scope item 3 (offline peer / can't reach admin): the join never observed the
    // admin within `timeout` — distinct from an authorization rejection.
    polled.map_err(|_| {
        crate::error::CliError::new(
            ErrorCode::NoAdminReachable,
            format!("could not bootstrap the room membership within {timeout:?}"),
        )
        .into()
    })
}

/// Wait (≤ `timeout`) until the node's snapshot shows `self_id` as `Active` — the
/// local confirmation that our own published join folded to membership.
async fn wait_for_active(node: &Node, self_id: &IdentityKey, timeout: Duration) -> Result<()> {
    let polled = tokio::time::timeout(timeout, async {
        loop {
            if let Ok(snapshot) = node.snapshot().await {
                if snapshot.is_active(self_id) {
                    return;
                }
            }
            tokio::time::sleep(JOIN_POLL_INTERVAL).await;
        }
    })
    .await;
    polled.map_err(|_| {
        anyhow!(
            "published the join but did not observe the local Active transition within {timeout:?}"
        )
    })
}

/// Map a `gate_join` rejection to an actionable, secret-free user message (D6 step
/// 10). These are the deterministic on-log verdicts every peer reaches.
fn join_reject_message(reason: RejectReason) -> String {
    match reason {
        RejectReason::BadCapability => {
            "this ticket's secret or identity does not match the invite (bad_capability)".to_owned()
        }
        RejectReason::ExpiredInvite => "this invite has expired (expired_invite)".to_owned(),
        RejectReason::InsufficientRole => {
            "the ticket's role does not match the invite (insufficient_role)".to_owned()
        }
        other => format!("the room rejected the join ({})", other.code()),
    }
}

/// Resolve the room's display name from its pulled genesis `room.created`, when
/// available (best-effort; absent before the pull or on a decode hiccup).
fn room_name_from_store(store: &EventStore, room_id: &RoomId) -> Option<String> {
    let genesis = store.by_type(room_id, EventType::RoomCreated).ok()?;
    let stored = genesis.into_iter().next()?;
    let event = SignedEvent::decode(&stored.wire.signed).ok()?;
    match event.content {
        Content::RoomCreated(c) => Some(c.room_name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{build_bootstrap_dial_set, join_reject_message, DEFAULT_JOIN_TIMEOUT};
    use iroh_rooms::experimental::session::{EndpointAddr, SecretKey};
    use iroh_rooms_core::event::ids::RoomId;
    use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey};
    use iroh_rooms_core::event::RejectReason;
    use iroh_rooms_core::ticket::RoomInviteTicket;

    fn device_key(seed: u8) -> DeviceKey {
        DeviceKey::from_bytes(
            SecretKey::from_bytes(&[seed; 32])
                .public()
                .as_bytes()
                .to_owned(),
        )
    }

    fn a_ticket(discovery: Vec<DeviceKey>) -> RoomInviteTicket {
        RoomInviteTicket {
            room_id: RoomId::from_bytes([0x11; 32]),
            invite_id: [0x22; 16],
            capability_secret: [0x33; 16],
            invitee_key: IdentityKey::from_bytes([0x44; 32]),
            role: "member".to_owned(),
            expires_at: None,
            inviter_identity: IdentityKey::from_bytes([0x55; 32]),
            discovery,
        }
    }

    #[test]
    fn default_timeout_parses() {
        // The clap default must be a valid `--timeout` spec.
        assert!(crate::message::parse_timeout(DEFAULT_JOIN_TIMEOUT).is_ok());
    }

    #[test]
    fn dial_set_uses_bare_addr_without_matching_peer() {
        let dev = device_key(7);
        let ticket = a_ticket(vec![dev]);
        let set = build_bootstrap_dial_set(&ticket, &[]).unwrap();
        assert_eq!(set.len(), 1);
        // No `--peer` ⇒ a bare, discovery-resolved address (no socket hints).
        assert_eq!(set[0].ip_addrs().count(), 0);
    }

    #[test]
    fn dial_set_pairs_matching_peer_address() {
        let dev = device_key(7);
        let id = SecretKey::from_bytes(&[7u8; 32]).public();
        let addr = EndpointAddr::new(id).with_ip_addr("127.0.0.1:45001".parse().unwrap());
        let ticket = a_ticket(vec![dev]);
        let set = build_bootstrap_dial_set(&ticket, &[addr]).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].id, id);
        assert_eq!(set[0].ip_addrs().count(), 1);
    }

    #[test]
    fn dial_set_dedups_repeated_discovery_devices() {
        let dev = device_key(7);
        let ticket = a_ticket(vec![dev, dev]);
        let set = build_bootstrap_dial_set(&ticket, &[]).unwrap();
        assert_eq!(set.len(), 1, "repeated discovery devices must dedup");
    }

    #[test]
    fn reject_messages_are_actionable_and_secret_free() {
        let bad = join_reject_message(RejectReason::BadCapability);
        assert!(bad.contains("bad_capability"));
        assert!(join_reject_message(RejectReason::ExpiredInvite).contains("expired_invite"));
        assert!(join_reject_message(RejectReason::InsufficientRole).contains("insufficient_role"));
    }

    #[test]
    fn dial_set_with_empty_discovery_returns_empty() {
        // No discovery hints + no --peer ⇒ empty; the caller bails with a
        // friendly "no admin discovery hint" message.
        let ticket = a_ticket(vec![]);
        let set = build_bootstrap_dial_set(&ticket, &[]).unwrap();
        assert!(
            set.is_empty(),
            "empty discovery + no --peer must yield an empty dial set"
        );
    }

    #[test]
    fn reject_message_catch_all_embeds_reason_code() {
        // RejectReason::NotAMember falls through to the `other` arm; the message
        // must contain the stable §8 code so log parsers and the user can act on it.
        let msg = join_reject_message(RejectReason::NotAMember);
        assert!(
            msg.contains("not_a_member"),
            "catch-all arm must embed the reason's §8 code"
        );
    }

    #[test]
    fn dial_set_mixed_pairing_independent_per_device() {
        // Two distinct discovery devices; only dev_a has a matching --peer hint.
        // The pairing must be per-device: dev_a gets the socket hint, dev_b is bare.
        let dev_a = device_key(7);
        let dev_b = device_key(8);
        let id_a = SecretKey::from_bytes(&[7u8; 32]).public();
        let id_b = SecretKey::from_bytes(&[8u8; 32]).public();
        let addr_a = EndpointAddr::new(id_a).with_ip_addr("127.0.0.1:45001".parse().unwrap());
        let ticket = a_ticket(vec![dev_a, dev_b]);
        let set = build_bootstrap_dial_set(&ticket, &[addr_a]).unwrap();
        assert_eq!(set.len(), 2, "two distinct devices → two entries");
        let a_entry = set
            .iter()
            .find(|a| a.id == id_a)
            .expect("dev_a must appear");
        assert_eq!(
            a_entry.ip_addrs().count(),
            1,
            "dev_a with matching --peer must carry the socket hint"
        );
        let b_entry = set
            .iter()
            .find(|a| a.id == id_b)
            .expect("dev_b must appear");
        assert_eq!(
            b_entry.ip_addrs().count(),
            0,
            "dev_b without --peer must be a bare discovery address"
        );
    }

    #[test]
    fn reject_message_unbound_device_uses_catch_all() {
        // UnboundDevice is a deferred reason that falls to the catch-all arm.
        // Its §8 code must appear so log-parsers and the user can act on it.
        let msg = join_reject_message(RejectReason::UnboundDevice);
        assert!(
            msg.contains("unbound_device"),
            "catch-all must embed the unbound_device §8 code; got: {msg}"
        );
    }

    #[test]
    fn all_named_reject_reasons_produce_non_empty_messages() {
        // Every RejectReason the join flow can encounter must produce a non-empty,
        // user-readable message — not an empty string or a raw Debug token.
        for msg in [
            join_reject_message(RejectReason::BadCapability),
            join_reject_message(RejectReason::ExpiredInvite),
            join_reject_message(RejectReason::InsufficientRole),
            join_reject_message(RejectReason::NotAMember),
            join_reject_message(RejectReason::UnboundDevice),
        ] {
            assert!(!msg.is_empty(), "rejection message must not be empty");
            // Must not expose raw binary or a bare Rust Debug representation.
            assert!(
                !msg.starts_with("RejectReason"),
                "must not expose a raw Debug token: {msg}"
            );
        }
    }
}
