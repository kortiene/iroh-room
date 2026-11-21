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

/// Maximum byte length of a file importable via `iroh-rooms file share`
/// (spec IR-0202 §5.7 / OQ-1). A deliberate MVP policy bound — **100 MiB** —
/// enforced against the file's metadata length *before* import, so an over-cap
/// file is rejected without being read. Import streams by path, so memory is not
/// the binding constraint; this cap protects the local blob store and keeps
/// `file list` / sync bounded. It is a `u64` to compare directly against
/// `std::fs::Metadata::len`.
pub const MAX_SHARED_FILE_BYTES: u64 = 104_857_600;

/// Maximum byte length of a `file.shared` display `name`. 255 matches the common
/// POSIX `NAME_MAX` and is far above any realistic file name; a longer name is
/// rejected as `invalid_content` at the stateless boundary.
pub const MAX_FILE_NAME_BYTES: usize = 255;

/// Maximum byte length of a `file.shared` `mime_type`. Registered media types are
/// short; 255 bounds the field without fighting long parameterized types.
pub const MAX_MIME_TYPE_BYTES: usize = 255;

/// Maximum number of asserted `providers` (`EndpointId`s) on a `file.shared`.
/// MVP rooms are ~3 members; 16 is a generous bound that blocks an unbounded array.
pub const MAX_FILE_PROVIDERS: usize = 16;

/// Maximum UTF-8 byte length of an `agent.status` `status` label (Event Protocol
/// §7). A short free-form label (e.g. `"running_tests"`), not a chat body.
pub const MAX_STATUS_LABEL_BYTES: usize = 64;

/// Maximum UTF-8 byte length of an `agent.status` `message` (Event Protocol §7).
/// A status note, not a chat transcript.
pub const MAX_STATUS_MESSAGE_BYTES: usize = 4096;

/// Maximum number of `agent.status` `related_artifact_ids` entries (Event
/// Protocol §7). Mirrors [`MAX_FILE_PROVIDERS`].
pub const MAX_ARTIFACT_REFS: usize = 16;

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

#[cfg(test)]
mod tests {
    use super::MAX_SHARED_FILE_BYTES;

    /// Doc/code guard: the shipped file-share cap is exactly 100 MiB. The PRD
    /// success metrics (PRD §18.6, PRD.v0.3 §17.1.9) and `RELEASE-READINESS.md`
    /// document this number verbatim; an earlier "25 MB" target that was never
    /// enforced diverged from the constant unnoticed. If this value ever
    /// changes, those docs must change with it — this assertion makes the drift
    /// loud instead of silent.
    #[test]
    fn shared_file_cap_is_100_mib() {
        assert_eq!(MAX_SHARED_FILE_BYTES, 100 * 1024 * 1024);
        assert_eq!(MAX_SHARED_FILE_BYTES, 104_857_600);
    }
}
