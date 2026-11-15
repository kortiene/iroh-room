//! Membership departures: the orchestration behind
//! `iroh-rooms room remove <ROOM_ID> <MEMBER_ID>` (admin kick, `member.removed`)
//! and `iroh-rooms room leave <ROOM_ID>` (voluntary self-leave, `member.left`).
//!
//! Both follow the `room send` pattern ([`crate::message::send`]): validate
//! arguments before any IO, fold the persisted log, build the event with the
//! SDK façade builder ([`iroh_rooms::room::build_member_removed`] /
//! [`iroh_rooms::room::build_member_left`]), self-validate it, then — when other
//! members are reachable — bring up an ephemeral node and let the engine's
//! `publish` persist and fan it out. The event is **always** persisted locally;
//! live delivery is best-effort (PRD §14).
//!
//! No new protocol rule: authority (only the admin may remove; anyone may
//! leave), sticky departure, and peer-side enforcement (connect-time
//! `NotActive` rejection, link teardown, blob/pipe gates, terminal-removal
//! delivery) are the conformance-tested core/net behavior this module drives.
//!
//! `room remove` pushes through a managed room session. The dial set is the
//! pre-removal roster, so the removed device is still reached with its
//! terminal `member.removed`. Once that event is folded, the session's live
//! admission and peer manager drop the device before the flush window, and
//! nothing accepted afterwards is forwarded to it.
//!
//! `room leave` can be run again after it has stored a `member.left`. The
//! retry does not author a second event: it re-delivers the stored one through
//! the ordinary membership sync.
//!
//! **What removal guarantees (v0.1 GA, plaintext content per ADR-0014 OD-1):**
//! removed members lose access to new messages, files and pipes once peers see
//! the removal; they keep what they already had. No content-key rotation runs
//! (`member.removed` carries `rotation: None`).

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use iroh_rooms::experimental::session::{EndpointAddr, SecretKey};
use iroh_rooms::room::{build_member_left, build_member_removed};
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::RejectReason;
use iroh_rooms_core::membership::{Ingest, MembershipSnapshot, RoomMembership, Status};
use iroh_rooms_core::store::EventStore;

use crate::display::{self, MemberDisplayState};
use crate::error::{CodedResultExt, ErrorCode};
use crate::identity;
use crate::message::{
    build_admission, build_dial_set, endpoint_id_of, fold_room, net_mode, parse_peers,
    run_managed_push, run_push, run_sync, select_heads, DB_FILE,
};

/// How a departure is pushed once it has been fold-checked.
#[derive(Clone, Copy)]
enum PushStyle {
    /// Fixed allowlist for the whole ephemeral session (`room leave`).
    Ephemeral,
    /// Live admission. The peer manager drops a device the fold has removed
    /// before the flush window (`room remove`).
    SealRoster,
}

/// Optional `reason` cap for a removal or leave (bytes). Mirrors the message
/// body cap order of magnitude while keeping the event small.
pub const MAX_REASON_BYTES: usize = 1024;

/// Which departure was authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepartureKind {
    /// Admin-authored `member.removed`.
    Removed,
    /// Self-authored `member.left`.
    Left,
}

/// The result of a `room remove` / `room leave`, for the caller to present.
pub struct DepartureSummary {
    /// Which event was authored.
    pub kind: DepartureKind,
    /// The authored event's id.
    pub event_id: EventId,
    /// The room.
    pub room_id: RoomId,
    /// The departing identity (the removed member, or the caller for leave).
    pub subject: IdentityKey,
    /// Number of connected peers the frame was pushed to (possibly zero).
    pub delivered: usize,
    /// Number of peers we tried to reach.
    pub attempted: usize,
    /// This call re-synced an already-stored `member.left` instead of authoring one.
    pub redelivered: bool,
    /// A repeat leave was given a `--reason` different from the stored event, and
    /// that stored reason was left unchanged.
    pub kept_original_reason: bool,
}

/// Remove `member_hex` from `room_id` (admin only) by authoring a
/// `member.removed`, persisting it, and best-effort pushing it to every other
/// member — including the removed member's device, so it learns its own removal.
///
/// # Errors
/// Fails — leaving the store untouched — if an argument is invalid, no local
/// identity exists, the room is unknown, the caller is not the room admin
/// (`insufficient_role`), the subject is the caller, unknown to the room, or
/// already removed (`not_a_member`, exit 3), or on a store error.
#[allow(clippy::too_many_arguments)] // one linear orchestration; each arg is a distinct CLI input
pub async fn remove(
    home: &Path,
    room_id: &RoomId,
    member_hex: &str,
    reason: Option<&str>,
    peers: &[String],
    timeout: Duration,
    loopback: bool,
) -> Result<DepartureSummary> {
    // ---- Pre-IO argument validation (a bad invocation writes nothing). ----
    let subject: IdentityKey = member_hex
        .trim()
        .parse()
        .map_err(|err| anyhow!("invalid member identity id (expected 64 hex characters): {err}"))
        .coded(ErrorCode::InvalidArgument)?;
    validate_reason(reason)?;
    let peer_addrs = parse_peers(peers)?;

    let secret = identity::SecretKeys::load(home)?;
    let caller = secret.identity.identity_key();
    if subject == caller {
        crate::bail_coded!(
            ErrorCode::InvalidArgument,
            "cannot remove yourself from room {room_id}; the room admin cannot leave or \
             remove itself"
        );
    }

    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (membership, snapshot) = fold_room(&store, home, room_id)?;

    if snapshot.admin() != Some(&caller) {
        crate::bail_coded!(
            ErrorCode::Reject(RejectReason::InsufficientRole),
            "only the room admin can remove members from {room_id} (this identity is {caller}); \
             to leave the room yourself, use `room leave`"
        );
    }
    match snapshot.member(&subject).map(|m| m.status) {
        None => crate::bail_coded!(
            ErrorCode::Reject(RejectReason::NotAMember),
            "{subject} is not a member of room {room_id}"
        ),
        Some(Status::Removed) => crate::bail_coded!(
            ErrorCode::Reject(RejectReason::NotAMember),
            "{subject} has already left or been removed from room {room_id}"
        ),
        Some(Status::Active | Status::Invited) => {}
    }

    let heads = select_heads(&store, room_id)?;
    let wire = build_member_removed(
        &secret.identity,
        &secret.device,
        room_id,
        &subject,
        reason,
        None,
        &heads,
        crate::clock::now_ms(),
    );
    let (event_id, delivered, attempted) = persist_and_push(
        home,
        store,
        membership,
        &snapshot,
        &secret,
        room_id,
        wire.to_bytes(),
        "member.removed",
        &peer_addrs,
        timeout,
        loopback,
        PushStyle::SealRoster,
    )
    .await?;
    Ok(DepartureSummary {
        kind: DepartureKind::Removed,
        event_id,
        room_id: *room_id,
        subject,
        delivered,
        attempted,
        redelivered: false,
        kept_original_reason: false,
    })
}

/// Leave `room_id` voluntarily by authoring a self-signed `member.left`,
/// persisting it, and best-effort pushing it to the other members.
///
/// If this identity already stored its own `member.left` (peers were offline
/// the first time), a later `room leave` re-delivers that same event and does
/// not author another one. A different `--reason` on that retry is not applied;
/// stdout says the existing event was re-synced and the original reason was kept.
///
/// # Errors
/// Fails — leaving the store untouched — if an argument is invalid, no local
/// identity exists, the room is unknown, the caller is not an active member and
/// has no stored self-leave to re-deliver, the caller is the room admin (the
/// single immutable admin cannot leave), or on a store error.
pub async fn leave(
    home: &Path,
    room_id: &RoomId,
    reason: Option<&str>,
    peers: &[String],
    timeout: Duration,
    loopback: bool,
) -> Result<DepartureSummary> {
    validate_reason(reason)?;
    let peer_addrs = parse_peers(peers)?;

    let secret = identity::SecretKeys::load(home)?;
    let caller = secret.identity.identity_key();

    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (membership, snapshot) = fold_room(&store, home, room_id)?;

    if snapshot.admin() == Some(&caller) {
        crate::bail_coded!(
            ErrorCode::InvalidArgument,
            "the room admin cannot leave room {room_id}: v1 rooms have a single immutable admin, \
             and the room would have no one left to invite or remove members"
        );
    }
    if !snapshot.is_active(&caller) {
        return redeliver_pending_leave(
            home,
            store,
            &snapshot,
            &secret,
            room_id,
            &caller,
            reason,
            &peer_addrs,
            timeout,
            loopback,
        )
        .await;
    }

    let heads = select_heads(&store, room_id)?;
    let wire = build_member_left(
        &secret.identity,
        &secret.device,
        room_id,
        reason,
        &heads,
        crate::clock::now_ms(),
    );
    let (event_id, delivered, attempted) = persist_and_push(
        home,
        store,
        membership,
        &snapshot,
        &secret,
        room_id,
        wire.to_bytes(),
        "member.left",
        &peer_addrs,
        timeout,
        loopback,
        PushStyle::Ephemeral,
    )
    .await?;
    Ok(DepartureSummary {
        kind: DepartureKind::Left,
        event_id,
        room_id: *room_id,
        subject: caller,
        delivered,
        attempted,
        redelivered: false,
        kept_original_reason: false,
    })
}

/// Re-deliver the caller's already-stored `member.left` without authoring
/// another. Fails with `not_a_member` when this identity did not leave itself
/// (unknown, invited, or admin-removed).
#[allow(clippy::too_many_arguments)] // distinct inputs of one linear flow
async fn redeliver_pending_leave(
    home: &Path,
    store: EventStore,
    snapshot: &MembershipSnapshot,
    secret: &identity::SecretKeys,
    room_id: &RoomId,
    caller: &IdentityKey,
    reason: Option<&str>,
    peer_addrs: &[EndpointAddr],
    timeout: Duration,
    loopback: bool,
) -> Result<DepartureSummary> {
    let Some(pending) = pending_self_leave(&store, room_id, caller)? else {
        crate::bail_coded!(
            ErrorCode::Reject(RejectReason::NotAMember),
            "you are not an active member of room {room_id} (this identity is {caller})"
        );
    };
    if !left_itself(snapshot, &store, room_id, caller)? {
        crate::bail_coded!(
            ErrorCode::Reject(RejectReason::NotAMember),
            "you are not an active member of room {room_id} (this identity is {caller})"
        );
    }
    let event_id = pending.event_id;
    // A different `--reason` does not rewrite the stored event.
    let kept_original_reason = reason.is_some_and(|given| pending.reason.as_deref() != Some(given));

    let self_device = endpoint_id_of(secret.device.device_key())?;
    let dial_set = build_dial_set(snapshot, self_device, peer_addrs);
    let attempted = dial_set.len();
    let delivered = if dial_set.is_empty() {
        0
    } else {
        let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
        let admission = build_admission(snapshot);
        match Box::pin(run_sync(
            home,
            store,
            room_id,
            secret_key,
            secret.device_seed(),
            admission,
            dial_set,
            timeout,
            net_mode(loopback),
        ))
        .await
        {
            Ok(n) => n,
            Err(err) => {
                eprintln!("warning: live delivery unavailable: {err:#}");
                0
            }
        }
    };
    Ok(DepartureSummary {
        kind: DepartureKind::Left,
        event_id,
        room_id: *room_id,
        subject: *caller,
        delivered,
        attempted,
        redelivered: true,
        kept_original_reason,
    })
}

/// Whether `caller`'s folded departure is their own `member.left` and not an
/// admin `member.removed` (a removal dominates a concurrent self-leave).
fn left_itself(
    snapshot: &MembershipSnapshot,
    store: &EventStore,
    room_id: &RoomId,
    caller: &IdentityKey,
) -> Result<bool> {
    let Some(status) = snapshot.status(caller) else {
        return Ok(false);
    };
    let (removed_ids, left_ids) = display::departure_sets(store, room_id)?;
    Ok(
        display::member_display_state(status, caller, &removed_ids, &left_ids)
            == MemberDisplayState::Left,
    )
}

/// One stored self-authored `member.left`, plus the reason it was signed with.
struct PendingLeave {
    event_id: EventId,
    reason: Option<String>,
    lamport: u64,
}

/// The newest self-authored `member.left` for `caller` in `room_id`, if any.
fn pending_self_leave(
    store: &EventStore,
    room_id: &RoomId,
    caller: &IdentityKey,
) -> Result<Option<PendingLeave>> {
    let rows = store
        .by_type(room_id, EventType::MemberLeft)
        .with_context(|| format!("could not read member.left events for room {room_id}"))?;
    let mut best: Option<PendingLeave> = None;
    for stored in rows {
        let Ok(event) = SignedEvent::decode(&stored.wire.signed) else {
            continue;
        };
        let Content::MemberLeft(content) = event.content else {
            continue;
        };
        if content.member_id != *caller || event.sender_id != *caller {
            continue;
        }
        let lamport = stored.lamport.unwrap_or(0);
        let newer = best
            .as_ref()
            .is_none_or(|current| (lamport, stored.event_id) > (current.lamport, current.event_id));
        if newer {
            best = Some(PendingLeave {
                event_id: stored.event_id,
                reason: content.reason,
                lamport,
            });
        }
    }
    Ok(best)
}

/// Self-validate and fold-check a freshly built departure, then persist it and
/// best-effort push it to every device the **pre-departure** snapshot lists as
/// active (minus our own), so a removed member's device is reached too and
/// receives its terminal removal. [`PushStyle::SealRoster`] then drops that
/// device before the flush. Returns `(event_id, delivered, attempted)`.
#[allow(clippy::too_many_arguments)] // distinct inputs of one linear flow
async fn persist_and_push(
    home: &Path,
    mut store: EventStore,
    mut membership: RoomMembership,
    snapshot: &MembershipSnapshot,
    secret: &identity::SecretKeys,
    room_id: &RoomId,
    wire_bytes: Vec<u8>,
    label: &str,
    peer_addrs: &[EndpointAddr],
    timeout: Duration,
    loopback: bool,
    style: PushStyle,
) -> Result<(EventId, usize, usize)> {
    let ctx = ValidationContext::for_room(*room_id);
    let validated = validate_wire_bytes(&wire_bytes, &ctx)
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built {label} failed validation ({})",
                reason.code()
            )
        })
        .coded(ErrorCode::Internal)?;
    let event_id = validated.event_id;
    match membership.ingest(validated.clone()) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => crate::bail_coded!(
            ErrorCode::Reject(reason.clone()),
            "{label} was rejected by the membership rules ({})",
            reason.code()
        ),
        Ingest::Buffered { .. } => crate::bail_coded!(
            ErrorCode::Internal,
            "internal error: freshly built {label} is causally incomplete"
        ),
    }

    let db_path = home.join(DB_FILE);
    let self_device = endpoint_id_of(secret.device.device_key())?;
    let dial_set = build_dial_set(snapshot, self_device, peer_addrs);
    let attempted = dial_set.len();

    let delivered = if dial_set.is_empty() {
        store
            .insert(&validated)
            .with_context(|| format!("could not persist {label} to {}", db_path.display()))?;
        0
    } else {
        let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
        let mode = net_mode(loopback);
        let device_seed = secret.device_seed();
        let pushed = match style {
            PushStyle::Ephemeral => {
                let admission = build_admission(snapshot);
                Box::pin(run_push(
                    home,
                    store,
                    room_id,
                    secret_key,
                    device_seed,
                    admission,
                    dial_set,
                    timeout,
                    mode,
                    wire_bytes,
                ))
                .await
            }
            PushStyle::SealRoster => {
                Box::pin(run_managed_push(
                    home,
                    store,
                    room_id,
                    secret_key,
                    device_seed,
                    dial_set,
                    timeout,
                    mode,
                    wire_bytes,
                ))
                .await
            }
        };
        let delivered = match pushed {
            Ok(n) => n,
            Err(err) => {
                eprintln!("warning: live delivery unavailable: {err:#}");
                0
            }
        };
        // Guarantee local persistence regardless of the push outcome (idempotent).
        let mut store = EventStore::open(&db_path)
            .with_context(|| format!("could not reopen event store at {}", db_path.display()))?;
        store
            .insert(&validated)
            .with_context(|| format!("could not persist {label} to {}", db_path.display()))?;
        delivered
    };
    Ok((event_id, delivered, attempted))
}

/// Validate an optional free-text reason before any IO.
fn validate_reason(reason: Option<&str>) -> Result<()> {
    if let Some(r) = reason {
        if r.is_empty() || r.len() > MAX_REASON_BYTES || r.chars().any(char::is_control) {
            crate::bail_coded!(
                ErrorCode::InvalidArgument,
                "--reason must be 1..={MAX_REASON_BYTES} bytes with no control characters"
            );
        }
    }
    Ok(())
}

/// Print a departure summary, with the honest v0.1 GA wording for removals.
pub fn print_departure(summary: &DepartureSummary) {
    match summary.kind {
        DepartureKind::Removed => println!("removed: {}", summary.subject),
        DepartureKind::Left => println!("left: {}", summary.room_id),
    }
    if summary.redelivered {
        println!("resync: re-synced the existing member.left");
        if summary.kept_original_reason {
            println!("reason: kept the original");
        }
    }
    println!("event: {}", summary.event_id);
    println!("room: {}", summary.room_id);
    println!("stored: yes");
    if summary.delivered == 0 {
        if summary.attempted == 0 {
            println!("delivered: 0 (no other members to reach — stored locally only)");
        } else if summary.kind == DepartureKind::Left {
            println!(
                "delivered: 0 (no peers online — stored locally only; run `room leave` again \
                 when peers are reachable to re-deliver this same member.left)"
            );
        } else {
            println!(
                "delivered: 0 (no peers online — the removal is stored; run `room tail` to sync \
                 it to peers)"
            );
        }
    } else {
        println!("delivered: {} connected peer(s)", summary.delivered);
    }
    if summary.kind == DepartureKind::Removed {
        println!(
            "note: removed members lose access to new messages, files and pipes once peers see \
             the removal; they keep what they already had (room content is not end-to-end \
             encrypted)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::validate_reason;

    #[test]
    fn reason_none_is_ok() {
        assert!(validate_reason(None).is_ok());
    }

    #[test]
    fn reason_rejects_empty_control_and_oversize() {
        assert!(validate_reason(Some("")).is_err());
        assert!(validate_reason(Some("a\nb")).is_err());
        assert!(validate_reason(Some(&"x".repeat(super::MAX_REASON_BYTES + 1))).is_err());
        assert!(validate_reason(Some("spam")).is_ok());
    }
}

/// Live loopback proofs for the two departure fixes: a third member's send
/// during the removal flush never reaches the removed device, and a `member.left`
/// stored while peers are offline is re-delivered exactly once when they return.
#[cfg(test)]
mod live {
    use std::path::Path;
    use std::str::FromStr;
    use std::time::Duration;

    use iroh_rooms::events::build_message_text;
    use iroh_rooms::experimental::session::{EndpointId, SecretKey};
    use iroh_rooms::identity::DeviceBinding;
    use iroh_rooms::room::{build_member_joined, RoomInviteTicket};
    use iroh_rooms_core::event::content::EventType;
    use iroh_rooms_core::event::ids::{EventId, RoomId};
    use iroh_rooms_core::event::keys::IdentityKey;
    use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_core::store::EventStore;
    use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
    use iroh_rooms_net::{
        AllowlistAdmission, NetConfig, NetMode, Node, TracingAudit, DEFAULT_TICK,
    };
    use tokio::sync::broadcast;

    use super::{leave, remove};
    use crate::identity::{self, SecretKeys};
    use crate::message::{self, endpoint_id_of, render_endpoint_addr, DB_FILE};

    struct Party {
        home: tempfile::TempDir,
        identity: IdentityKey,
        device: EndpointId,
    }

    fn party(name: &str) -> Party {
        let home = tempfile::TempDir::new().expect("temp home");
        identity::create(home.path(), name, false).expect("identity");
        let secret = SecretKeys::load(home.path()).expect("keys");
        Party {
            device: endpoint_id_of(secret.device.device_key()).expect("device id"),
            identity: secret.identity.identity_key(),
            home,
        }
    }

    fn invite_and_join(admin: &Party, member: &Party, room: &RoomId) {
        let summary = crate::invite::invite(
            admin.home.path(),
            room,
            &member.identity.to_string(),
            "member",
            None,
        )
        .expect("invite");
        let ticket = RoomInviteTicket::from_str(&summary.ticket).expect("ticket");
        let secret = SecretKeys::load(member.home.path()).expect("member keys");
        let heads = EventStore::open(&admin.home.path().join(DB_FILE))
            .expect("admin store")
            .heads(room)
            .expect("heads");
        let binding = DeviceBinding::create(room, &secret.identity, secret.device.device_key());
        let wire = build_member_joined(
            &secret.identity,
            &secret.device,
            room,
            &ticket.invite_id,
            &ticket.capability_secret,
            &ticket.role,
            binding,
            None,
            &heads,
            crate::clock::now_ms(),
        );
        let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(*room))
            .expect("join validates");
        EventStore::open(&admin.home.path().join(DB_FILE))
            .expect("admin store")
            .insert(&validated)
            .expect("insert join");
    }

    fn replicate(from: &Party, to: &Party, room: &RoomId) {
        let src = EventStore::open(&from.home.path().join(DB_FILE)).expect("src");
        let mut dst = EventStore::open(&to.home.path().join(DB_FILE)).expect("dst");
        let ctx = ValidationContext::for_room(*room);
        for id in src.room_event_ids(room).expect("ids") {
            let bytes = src.get(&id).expect("get").expect("row").wire.to_bytes();
            let validated = validate_wire_bytes(&bytes, &ctx).expect("revalidate");
            dst.insert(&validated).expect("replicate");
        }
    }

    fn count_type(home: &Path, room: &RoomId, ty: EventType) -> usize {
        EventStore::open(&home.join(DB_FILE))
            .expect("store")
            .by_type(room, ty)
            .expect("by_type")
            .len()
    }

    fn joined_room(names: &[&str]) -> (Party, Vec<Party>, RoomId) {
        let admin = party("Alice");
        let created = crate::room::create(admin.home.path(), "Departure").expect("room");
        let members: Vec<Party> = names.iter().map(|name| party(name)).collect();
        for member in &members {
            invite_and_join(&admin, member, &created.room_id);
        }
        for member in &members {
            replicate(&admin, member, &created.room_id);
        }
        (admin, members, created.room_id)
    }

    fn spawn_listener<'a>(
        party: &'a Party,
        room: RoomId,
        admit: &'a [&'a Party],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Node> + Send + 'a>> {
        Box::pin(async move {
            let secret = SecretKeys::load(party.home.path()).expect("keys");
            let store = EventStore::open(&party.home.path().join(DB_FILE)).expect("store");
            let engine = SyncEngine::open_with_local_device(
                store,
                room,
                SyncConfig::default(),
                Some(secret.device_seed()),
            )
            .expect("engine");
            let mut auth = AllowlistAdmission::new();
            for peer in admit {
                auth = auth
                    .bind_device(peer.device, peer.identity)
                    .set_active(peer.identity);
            }
            Node::spawn(
                SecretKey::from_bytes(&secret.device_seed()),
                std::sync::Arc::new(auth),
                std::sync::Arc::new(TracingAudit),
                engine,
                NetConfig {
                    mode: NetMode::Loopback,
                    ..NetConfig::default()
                },
                DEFAULT_TICK,
            )
            .await
            .expect("listener")
        })
    }

    fn hold_flush_open() {
        message::FLUSH_GRACE_OVERRIDE_MS.store(4_000, std::sync::atomic::Ordering::Relaxed);
    }

    /// A third member sends after the removal is folded, while the admin's push
    /// node is still in its flush window. The removed device gets the terminal
    /// removal and not that send.
    #[tokio::test]
    async fn removal_flush_does_not_forward_a_third_members_send() {
        hold_flush_open();
        let (alice, members, room) = joined_room(&["Bob", "Carol"]);
        let (bob, carol) = (&members[0], &members[1]);
        let admit: [&Party; 3] = [&alice, bob, carol];
        let bob_node = spawn_listener(bob, room, &admit).await;
        let carol_node = spawn_listener(carol, room, &admit).await;
        let peers = vec![
            render_endpoint_addr(&bob_node.endpoint_addr().expect("bob addr")),
            render_endpoint_addr(&carol_node.endpoint_addr().expect("carol addr")),
        ];
        let carol_keys = SecretKeys::load(carol.home.path()).expect("carol keys");
        let mut carol_events = carol_node.room_events();
        let bob_hex = bob.identity.to_string();

        let mut remove_fut = Box::pin(remove(
            alice.home.path(),
            &room,
            &bob_hex,
            None,
            &peers,
            Duration::from_secs(5),
            true,
        ));
        let mut message_id = None;
        let summary = loop {
            tokio::select! {
                summary = &mut remove_fut => break summary.expect("room remove"),
                recv = carol_events.recv() => {
                    if message_id.is_none() {
                        if let Some(id) =
                            publish_during_flush(&carol_node, &carol_keys, room, recv).await
                        {
                            message_id = Some(id);
                        }
                    }
                }
            }
        };
        let message_id = message_id.expect("carol must send during the removal flush");
        tokio::time::sleep(Duration::from_millis(400)).await;

        assert!(
            bob_node
                .store_contains(summary.event_id)
                .await
                .expect("bob read"),
            "the removed device still receives its terminal member.removed"
        );
        assert!(
            !bob_node.store_contains(message_id).await.expect("bob read"),
            "a send accepted during the flush must not be forwarded to the removed device"
        );
        assert!(
            store_has(&alice.home.path().join(DB_FILE), message_id),
            "the removal author must have accepted the third member's send"
        );
        assert_eq!(
            events_of_type(&bob_node, EventType::MemberRemoved).await,
            1,
            "the removed device stores the removal once"
        );

        bob_node.shutdown().await.expect("shutdown bob");
        carol_node.shutdown().await.expect("shutdown carol");
    }

    async fn publish_during_flush(
        carol: &Node,
        keys: &SecretKeys,
        room: RoomId,
        recv: Result<iroh_rooms_core::store::StoredEvent, broadcast::error::RecvError>,
    ) -> Option<EventId> {
        let event = match recv {
            Ok(event) if event.event_type == EventType::MemberRemoved => event,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => return None,
            Err(broadcast::error::RecvError::Closed) => {
                panic!("carol's room-event stream closed before the removal")
            }
        };
        let wire = build_message_text(
            &keys.identity,
            &keys.device,
            &room,
            "during-flush",
            None,
            None,
            &[],
            &[event.event_id],
            crate::clock::now_ms(),
        );
        let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room))
            .expect("carol's message validates");
        carol
            .publish(wire.to_bytes())
            .await
            .expect("carol publishes during the flush");
        Some(validated.event_id)
    }

    fn store_has(path: &Path, id: EventId) -> bool {
        EventStore::open(path)
            .expect("store")
            .contains(&id)
            .expect("contains")
    }

    async fn events_of_type(node: &Node, ty: EventType) -> usize {
        node.room_tail(10_000)
            .await
            .expect("tail")
            .into_iter()
            .filter(|event| event.event_type == ty)
            .count()
    }

    /// Peers are down when `room leave` runs. When one comes back, a second
    /// `room leave` delivers that same `member.left` and does not author another.
    #[tokio::test]
    async fn leave_redelivers_exactly_one_member_left_when_peers_return() {
        hold_flush_open();
        let (alice, members, room) = joined_room(&["Bob"]);
        let bob = &members[0];

        let first = leave(
            bob.home.path(),
            &room,
            None,
            &[],
            Duration::from_millis(300),
            true,
        )
        .await
        .expect("leave while peers are offline");
        assert_eq!(first.delivered, 0, "nobody was online to take the leave");
        assert_eq!(count_type(bob.home.path(), &room, EventType::MemberLeft), 1);

        let alice_node = spawn_listener(&alice, room, &[&alice, bob]).await;
        let peers = vec![render_endpoint_addr(
            &alice_node.endpoint_addr().expect("alice addr"),
        )];
        let second = leave(
            bob.home.path(),
            &room,
            Some("ignored on retry"),
            &peers,
            Duration::from_secs(5),
            true,
        )
        .await
        .expect("leave retries the stored departure");

        assert_eq!(second.event_id, first.event_id);
        assert!(
            second.delivered >= 1,
            "the returned peer must be connected for the re-delivery"
        );
        assert_eq!(
            count_type(bob.home.path(), &room, EventType::MemberLeft),
            1,
            "the retry must not author a second member.left"
        );
        assert!(
            alice_node
                .store_contains(first.event_id)
                .await
                .expect("alice read"),
            "the peer that came back receives the stored member.left"
        );
        assert_eq!(
            events_of_type(&alice_node, EventType::MemberLeft).await,
            1,
            "the peer stores exactly one member.left"
        );
        alice_node.shutdown().await.expect("shutdown alice");
    }
}
