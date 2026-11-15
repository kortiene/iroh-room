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
//! **What removal guarantees (v0.1 GA, plaintext content per ADR-0014 OD-1):**
//! removed members lose access to new messages, files and pipes once peers see
//! the removal; they keep what they already had. No content-key rotation runs
//! (`member.removed` carries `rotation: None`).

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use iroh_rooms::experimental::session::SecretKey;
use iroh_rooms::room::{build_member_left, build_member_removed};
use iroh_rooms_core::event::ids::{EventId, RoomId};
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::RejectReason;
use iroh_rooms_core::membership::{Ingest, MembershipSnapshot, RoomMembership, Status};
use iroh_rooms_core::store::EventStore;

use crate::error::{CodedResultExt, ErrorCode};
use crate::identity;
use crate::message::{
    build_admission, build_dial_set, endpoint_id_of, fold_room, net_mode, parse_peers, run_push,
    select_heads, DB_FILE,
};

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
}

/// Remove `member_hex` from `room_id` (admin only) by authoring a
/// `member.removed`, persisting it, and best-effort pushing it to every other
/// member — including the removed member's device, so it learns its own removal.
///
/// # Errors
/// Fails — leaving the store untouched — if an argument is invalid, no local
/// identity exists, the room is unknown, the caller is not the room admin
/// (`insufficient_role`), the subject is the caller, unknown to the room, or
/// already removed, or on a store error.
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
        .map_err(|err| {
            anyhow!("invalid member identity id (expected 64-char lowercase hex): {err}")
        })
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
            ErrorCode::InvalidArgument,
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
    )
    .await?;
    Ok(DepartureSummary {
        kind: DepartureKind::Removed,
        event_id,
        room_id: *room_id,
        subject,
        delivered,
        attempted,
    })
}

/// Leave `room_id` voluntarily by authoring a self-signed `member.left`,
/// persisting it, and best-effort pushing it to the other members.
///
/// # Errors
/// Fails — leaving the store untouched — if an argument is invalid, no local
/// identity exists, the room is unknown, the caller is not an active member, the
/// caller is the room admin (the single immutable admin cannot leave), or on a
/// store error.
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
        crate::bail_coded!(
            ErrorCode::Reject(RejectReason::NotAMember),
            "you are not an active member of room {room_id} (this identity is {caller})"
        );
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
    )
    .await?;
    Ok(DepartureSummary {
        kind: DepartureKind::Left,
        event_id,
        room_id: *room_id,
        subject: caller,
        delivered,
        attempted,
    })
}

/// Self-validate and fold-check a freshly built departure, then persist it and
/// best-effort push it to every device the **pre-departure** snapshot lists as
/// active (minus our own), so a removed member's device is reached too and
/// receives its terminal removal. Returns `(event_id, delivered, attempted)`.
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
    peer_addrs: &[iroh_rooms::experimental::session::EndpointAddr],
    timeout: Duration,
    loopback: bool,
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
        let admission = build_admission(snapshot);
        let delivered = match Box::pin(run_push(
            home,
            store,
            room_id,
            secret_key,
            secret.device_seed(),
            admission,
            dial_set,
            timeout,
            net_mode(loopback),
            wire_bytes,
        ))
        .await
        {
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
    println!("event: {}", summary.event_id);
    println!("room: {}", summary.room_id);
    println!("stored: yes");
    if summary.delivered == 0 {
        if summary.attempted == 0 {
            println!("delivered: 0 (no other members to reach — stored locally only)");
        } else {
            println!(
                "delivered: 0 (no peers online — stored locally only; peers learn it when they \
                 next sync with you)"
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
