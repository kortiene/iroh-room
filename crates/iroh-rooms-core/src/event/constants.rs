//! Protocol constants: domain-separation contexts and structural limits.
//!
//! All context strings are ASCII with no embedded NUL, taken verbatim from
//! `PHASE-0-SPIKE.md` Event Protocol §1, §4, §5, §6. They are domain separators
//! for the various signatures/hashes so a signature in one role can never be
//! replayed in another.

/// Domain separator prepended to the canonical signed bytes before signing
/// (Event Protocol §6): `sig_msg = EVENT_CONTEXT ‖ CSB`.
pub const EVENT_CONTEXT: &[u8] = b"iroh-rooms:event:v1";

/// Domain separator for room-ID derivation (Event Protocol §5).
pub const ROOMID_CONTEXT: &[u8] = b"iroh-rooms:room-id:v1";

/// Domain separator for the device-binding certificate (Event Protocol §1):
/// `binding_msg = BIND_CONTEXT ‖ room_id ‖ sender_id ‖ device_id`.
pub const BIND_CONTEXT: &[u8] = b"iroh-rooms:device-binding:v1";

/// Domain separator for invite capability hashes (Event Protocol §7):
/// `capability_hash = BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)`.
pub const INVITE_CONTEXT: &[u8] = b"iroh-rooms:invite:v1";

/// Maximum number of causal parents an event may declare (Event Protocol §2/§6
/// step 9). Bounds DAG fan-in and inbound allocation.
pub const MAX_PREV_EVENTS: usize = 20;

/// Maximum UTF-8 byte length of a `message.text` body (Event Protocol §7).
pub const MAX_MESSAGE_BODY_BYTES: usize = 16_384;

/// The only accepted logical `schema_version` for MVP (Event Protocol §2).
pub const SCHEMA_VERSION: u64 = 1;

/// The only accepted `WireEvent` envelope version for MVP (Event Protocol §3).
pub const WIRE_VERSION: u64 = 1;

/// Advisory clock-skew threshold in milliseconds (Event Protocol §6 step 10).
/// A `created_at` more than this far ahead of local time raises an advisory
/// flag and is **never** a rejection.
pub const CLOCK_SKEW_FUTURE_MS: u64 = 300_000;

/// Length in bytes of an Ed25519 public key (`sender_id`, `device_id`).
pub const PUBLIC_KEY_LEN: usize = 32;

/// Length in bytes of an Ed25519 detached signature.
pub const SIGNATURE_LEN: usize = 64;

/// Length in bytes of a BLAKE3-256 digest (`event_id`, `room_id`, blob hashes).
pub const DIGEST_LEN: usize = 32;

/// Length in bytes of the short opaque identifiers used in content schemas
/// (`invite_id`, `capability_secret`, `pipe_id`, `file_id`, artifact ids).
pub const SHORT_ID_LEN: usize = 16;
