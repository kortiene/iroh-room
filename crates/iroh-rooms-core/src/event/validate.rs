//! The stateless single-event verification pipeline (Event Protocol §6).
//!
//! [`validate_wire_bytes`] implements the **stateless subset** of the §6
//! algorithm — every check that depends only on the event's own bytes (and an
//! expected room context the caller supplies). Stateful steps (device binding
//! from membership state, membership & role authorization, transitive
//! genesis-descent, dedup/persist) are deferred and represented by the
//! [`MembershipOracle`](super::reject::MembershipOracle) trait boundary; they
//! are **not** decided here.
//!
//! On success it returns a [`ValidatedEvent`] carrying the decoded
//! [`SignedEvent`], the recomputed [`EventId`], the **verbatim** `signed` bytes
//! and full [`WireEvent`] (for byte-faithful storage/forwarding), and any
//! advisory [`Flag`]s. The first failing check returns a typed [`RejectReason`].

use super::cbor;
use super::constants::{CLOCK_SKEW_FUTURE_MS, MAX_PREV_EVENTS};
use super::content::{self, Content};
use super::ids::{EventId, RoomId};
use super::reject::{Flag, MembershipOracle, RejectReason};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Caller-supplied context for stateless validation.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    /// The room being processed (Event Protocol §6 step 6 / spec Open Q4). The
    /// event's `room_id` must equal this; for `room.created` the recomputed
    /// genesis id must also equal it. Provided by the caller's room context
    /// (e.g. the room they hold the genesis for, or the room id in an invite
    /// ticket) — never resolved from a store in this stateless layer.
    pub expected_room: RoomId,
    /// Optional "now" (ms since Unix epoch) for the advisory clock-skew check
    /// (§6 step 10). `None` keeps validation fully time-independent.
    pub now_ms: Option<u64>,
}

impl ValidationContext {
    /// Time-independent context for `expected_room` (no clock-skew check).
    #[must_use]
    pub fn for_room(expected_room: RoomId) -> Self {
        Self {
            expected_room,
            now_ms: None,
        }
    }
}

/// A successfully validated event: the decoded fields plus everything the
/// persistence/forwarding layers need to store and re-broadcast verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEvent {
    /// The recomputed event id (`BLAKE3-256(signed)`), the stable dedup key.
    pub event_id: EventId,
    /// The decoded, strictly-validated logical event.
    pub event: SignedEvent,
    /// The full transport envelope, preserved verbatim.
    pub wire: WireEvent,
    /// Advisory flags attached to this otherwise-accepted event.
    pub flags: Vec<Flag>,
}

impl ValidatedEvent {
    /// The verbatim canonical signed bytes (CSB) — the exact bytes that were
    /// hashed and verified, preserved for storage and forwarding.
    #[must_use]
    pub fn signed_bytes(&self) -> &[u8] {
        &self.wire.signed
    }
}

/// Run the stateless §6 pipeline over a `WireEvent`'s bytes.
///
/// # Errors
/// Returns the first applicable [`RejectReason`]:
/// `non_canonical_encoding`, `id_mismatch`, `bad_signature`,
/// `unknown_schema_version`, `unknown_event_type`, `invalid_content`,
/// `room_id_mismatch`, `too_many_parents`, or `not_genesis_descended`.
pub fn validate_wire_bytes(
    bytes: &[u8],
    ctx: &ValidationContext,
) -> Result<ValidatedEvent, RejectReason> {
    // Step 1 — decode transport (rejects v != 1, missing keys, non-canonical).
    let wire = WireEvent::decode(bytes)?;

    // Step 2 — recompute id from the EXACT signed bytes; never trust `wire.id`.
    let event_id = signed::event_id_from_bytes(&wire.signed);
    if event_id.to_named_string() != wire.id {
        return Err(RejectReason::IdMismatch);
    }

    // Step 4 (canonicality) — decode the signed bytes under the strict profile
    // and confirm they re-encode byte-for-byte. Done before the signature check
    // so crypto never runs on non-canonical input (a single-parser refinement of
    // the §6 step 3/4 order; the reason codes are unaffected — see spec D1/§8).
    let value =
        cbor::decode_canonical(&wire.signed).map_err(|_| RejectReason::NonCanonicalEncoding)?;
    if cbor::encode(&value) != wire.signed {
        return Err(RejectReason::NonCanonicalEncoding);
    }

    // Step 3 — verify the signature under `device_id` (NEVER `sender_id`).
    let device_id = signed::read_device_id(&value)?;
    let signing_message = signed::event_signing_message(&wire.signed);
    device_id
        .verify(&signing_message, &wire.sig)
        .map_err(|_| RejectReason::BadSignature)?;

    // Step 4 (shape) + Step 5 — exact eight typed keys, schema_version == 1,
    // registered event_type, strict per-type content validation.
    let event = SignedEvent::from_canonical_value(&value)?;

    // Step 5 (cont.) — per-type field rules needing only the sender identity.
    content::check_field_rules(&event.content, &event.sender_id)?;

    // Step 6 — room binding.
    match &event.content {
        Content::RoomCreated(c) => {
            let derived = signed::derive_room_id(&event.sender_id, &c.room_nonce, event.created_at);
            if derived != event.room_id || event.room_id != ctx.expected_room {
                return Err(RejectReason::RoomIdMismatch);
            }
        }
        _ => {
            if event.room_id != ctx.expected_room {
                return Err(RejectReason::RoomIdMismatch);
            }
        }
    }

    // Step 7 — self-contained device-binding verification (the three carrying
    // types). Resolving bindings from membership state is deferred.
    content::verify_bindings(
        &event.content,
        &event.sender_id,
        &event.device_id,
        &event.room_id,
    )?;

    // Step 9 — stateless causal structure. (Step 8 membership/role is deferred;
    // full transitive genesis-descent needs the DAG and is deferred.)
    if event.prev_events.len() > MAX_PREV_EVENTS {
        return Err(RejectReason::TooManyParents);
    }
    let is_genesis = matches!(event.content, Content::RoomCreated(_));
    if is_genesis {
        if !event.prev_events.is_empty() {
            return Err(RejectReason::NotGenesisDescended);
        }
    } else if event.prev_events.is_empty() {
        return Err(RejectReason::NotGenesisDescended);
    }

    // Step 10 — advisory clock-skew flag only (never rejects, reorders, drops).
    let mut flags = Vec::new();
    if let Some(now) = ctx.now_ms {
        if event.created_at > now.saturating_add(CLOCK_SKEW_FUTURE_MS) {
            flags.push(Flag::ClockSkew);
        }
    }

    Ok(ValidatedEvent {
        event_id,
        event,
        wire,
        flags,
    })
}

/// Run the stateless pipeline and then the **stateful** §6 steps 7–8 against a
/// [`MembershipOracle`] (Event Protocol §6; spec D3 / scope item 10).
///
/// This is the frozen-surface entry named in the [`MembershipOracle`] doc: it
/// completes [`validate_wire_bytes`] with the membership/role authorization
/// (step 8) and the membership-derived device binding (step 7) for the event
/// types that carry no self-contained binding
/// ([`EventType::requires_membership_device_binding`](super::content::EventType::requires_membership_device_binding)).
///
/// The `oracle` is expected to be an **ancestor-scoped** view of the event's
/// causal ancestors (the membership layer's `AncestorView`), so the verdict is
/// ancestor-stable and identical on every peer regardless of arrival order.
///
/// Authorization (step 8) is evaluated **before** the membership-derived device
/// binding (step 7) so a non-member yields the more specific `not_a_member`
/// rather than `unbound_device`; both are rejections, so the verdict is
/// unaffected (spec D3 / vector §13).
///
/// The `member.joined` capability check (key-bound invite liveness, log-only
/// expiry, sticky departure) **cannot** be expressed through the content-free
/// [`MembershipOracle::authorize`] signature and is therefore performed by the
/// membership fold's ingest path, not here (spec Open Q1 / R7).
///
/// # Errors
/// Returns any [`RejectReason`] from [`validate_wire_bytes`], or the deferred
/// `not_a_member` / `insufficient_role` (step 8) or `unbound_device` (step 7).
pub fn validate_with_membership(
    bytes: &[u8],
    ctx: &ValidationContext,
    oracle: &impl MembershipOracle,
) -> Result<ValidatedEvent, RejectReason> {
    let validated = validate_wire_bytes(bytes, ctx)?;
    let event = &validated.event;

    // Step 8 — membership & role authorization in the ancestor view.
    oracle.authorize(&event.room_id, &event.sender_id, event.event_type.as_str())?;

    // Step 7 — device binding from membership state, for the types that carry no
    // self-contained binding. A `None` bound device means the sender has no
    // membership-bound device (e.g. an inert `member.left` from a non-member);
    // such events are accepted (they grant nothing).
    if event.event_type.requires_membership_device_binding() {
        if let Some(bound) = oracle.bound_device(&event.room_id, &event.sender_id) {
            if &bound != event.device_id.as_bytes() {
                return Err(RejectReason::UnboundDevice);
            }
        }
    }

    Ok(validated)
}
