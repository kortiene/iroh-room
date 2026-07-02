//! The `agent` CLI noun: `iroh-rooms agent invite <ROOM_ID> <AGENT_ID> [--expires
//! <DURATION>]` and `iroh-rooms agent status <ROOM_ID> <STATUS> [...]` (spec
//! IR-0206 D1, IR-0208 D4).
//!
//! `agent invite` is a **façade**, not new authorization: an agent is an ordinary
//! principal distinguished only by `role = "agent"` (spike §1/§3.1), and the
//! admin-only, key-bound invite path is already landed (IR-0103,
//! [`crate::invite::invite`]). `agent invite` exists only so the PRD-documented
//! `agent invite <room-id> <agent-id>` surface is a first-class command instead
//! of a `--role agent` flag buried under `room invite` — every authorization
//! decision (admin gate, capability hash, ticket codec, `member.invited`
//! builder, IR-0110 error codes) is reused verbatim.
//!
//! `agent status` is likewise a thin validator over
//! [`crate::message::send_agent_status`] (spec IR-0208 D4): posting an
//! `agent.status` is **not** role-gated — any active member may post (the
//! `gate_active_member` fold check, matching Spike §7 "any current member"). This
//! module owns the pre-IO friendly checks (`status`/`message` caps, `progress`
//! bound, artifact-handle parsing/dedup) before delegating the
//! fold → heads → build → self-validate → persist → best-effort-push flow.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use iroh_rooms_core::event::constants::{
    MAX_ARTIFACT_REFS, MAX_STATUS_LABEL_BYTES, MAX_STATUS_MESSAGE_BYTES, SHORT_ID_LEN,
};
use iroh_rooms_core::event::ids::RoomId;

use crate::error::{CodedResultExt, ErrorCode};
use crate::invite::{self, InviteSummary};
use crate::message::{self, StatusSummary};

/// Register a known agent identity into a room as an `agent`-role member.
///
/// A thin wrapper over the landed key-bound invite path: it is exactly
/// `room invite <ROOM_ID> --invitee <AGENT_ID> --role agent [--expires …]`,
/// given the `agent` noun and positional shape PRD §16 documents. No new event
/// type, no new authorization — the admin gate, capability hash, ticket codec,
/// and `member.invited` builder are all reused verbatim (spec IR-0206 D1).
///
/// # Errors
/// Propagates every failure of [`crate::invite::invite`] unchanged (not admin,
/// no such room, self-invite, bad expiry, bad agent id, no local identity, …).
pub fn invite(
    home: &Path,
    room_id: &RoomId,
    agent_id: &str,
    expires: Option<&str>,
) -> Result<InviteSummary> {
    invite::invite(home, room_id, agent_id, "agent", expires)
}

/// Post a signed `agent.status` update: validate every field before any IO, then
/// delegate to [`crate::message::send_agent_status`] (spec IR-0208 D4). Any
/// active member may call this — the CLI noun is not a role gate.
///
/// # Errors
/// Fails with `InvalidArgument` before any IO if `status`/`message` is over the
/// content-parser's caps or contains control characters, if `progress` exceeds
/// 100, or if an `--artifact` value is not a well-formed file-id handle or there
/// are too many of them. Otherwise propagates every failure of
/// [`crate::message::send_agent_status`] unchanged (no local identity, unknown
/// room, not an active member, …).
#[allow(clippy::too_many_arguments)]
pub async fn status(
    home: &Path,
    room_id: &RoomId,
    status: &str,
    message: Option<&str>,
    progress: Option<u64>,
    artifacts: &[String],
    peers: &[String],
    timeout: Duration,
    loopback: bool,
) -> Result<StatusSummary> {
    validate_status(status).coded(ErrorCode::InvalidArgument)?;
    validate_message(message).coded(ErrorCode::InvalidArgument)?;
    if let Some(pct) = progress {
        if pct > 100 {
            crate::bail_coded!(
                ErrorCode::InvalidArgument,
                "--progress must be an integer 0..=100 (got {pct})"
            );
        }
    }
    let related_artifact_ids = parse_artifacts(artifacts).coded(ErrorCode::InvalidArgument)?;

    message::send_agent_status(
        home,
        room_id,
        status,
        message,
        progress,
        &related_artifact_ids,
        peers,
        timeout,
        loopback,
    )
    .await
}

/// Validate `--status`/the positional `<STATUS>`: non-empty, within the content
/// parser's cap, and no control characters — it renders directly into the tail
/// (mirrors `parse_agent_status`'s D1 bound).
fn validate_status(s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("status must not be empty");
    }
    let len = s.len();
    if len > MAX_STATUS_LABEL_BYTES {
        bail!("status must be at most {MAX_STATUS_LABEL_BYTES} bytes (got {len})");
    }
    if s.chars().any(char::is_control) {
        bail!("status must not contain control characters");
    }
    Ok(())
}

/// Validate `--message`: within the content parser's cap. Newlines/Unicode are
/// allowed (a human sentence), matching `message.text` body policy.
fn validate_message(m: Option<&str>) -> Result<()> {
    if let Some(m) = m {
        let len = m.len();
        if len > MAX_STATUS_MESSAGE_BYTES {
            bail!("message must be at most {MAX_STATUS_MESSAGE_BYTES} bytes (got {len})");
        }
    }
    Ok(())
}

/// Parse every `--artifact` value via the shared file-id handle codec
/// ([`crate::file::parse_file_id`]: `file_<32-hex>` or bare 32-hex), de-duplicate
/// (order-preserving), and enforce the content parser's count cap (spec IR-0208
/// D6).
fn parse_artifacts(raw: &[String]) -> Result<Vec<[u8; SHORT_ID_LEN]>> {
    let mut out: Vec<[u8; SHORT_ID_LEN]> = Vec::new();
    for s in raw {
        let id = crate::file::parse_file_id(s)?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    if out.len() > MAX_ARTIFACT_REFS {
        bail!(
            "too many --artifact values (max {MAX_ARTIFACT_REFS}, got {})",
            out.len()
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! Source-level proofs that the `agent` noun is a pure façade over the landed
    //! key-bound invite path (spec IR-0206 D1, risk R1). These exercise
    //! [`invite`] directly — no binary spawn — so the delegation contract is pinned
    //! at the library boundary the CLI integration suite only observes through
    //! printed output.

    use iroh_rooms_core::event::ids::RoomId;
    use tempfile::TempDir;

    use crate::{identity, room};

    /// A fixed 64-hex agent identity key. `IdentityKey::from_bytes` does not check
    /// curve-point membership, so any 32-byte hex is a well-formed invitee key for
    /// the offline invite path; this value never collides with a generated admin.
    const AGENT_HEX: &str = "0606060606060606060606060606060606060606060606060606060606060606";
    /// A second, distinct agent key for the delegation-equivalence test.
    const AGENT_HEX_B: &str = "0707070707070707070707070707070707070707070707070707070707070707";

    /// A temp home whose sole identity is the admin of a freshly created room.
    fn admin_home_with_room() -> (TempDir, RoomId) {
        let dir = TempDir::new().unwrap();
        identity::create(dir.path(), "Alice", false).unwrap();
        let summary = room::create(dir.path(), "Build Room").unwrap();
        (dir, summary.room_id)
    }

    /// `agent::invite` mints an `agent`-role invite bound to the exact agent key,
    /// scoped to the room, with a `roomtkt1…` ticket and no expiry by default.
    #[test]
    fn agent_invite_returns_summary_bound_to_agent_role() {
        let (home, room_id) = admin_home_with_room();
        let summary = super::invite(home.path(), &room_id, AGENT_HEX, None).unwrap();

        assert_eq!(summary.role, "agent", "the wrapper hard-codes role=agent");
        assert_eq!(
            summary.invitee_key.to_string(),
            AGENT_HEX,
            "the invite is key-bound to the agent id (AC2)"
        );
        assert_eq!(summary.room_id, room_id, "scoped to the target room");
        assert!(summary.expires_at.is_none(), "no expiry unless requested");
        assert!(
            summary.ticket.starts_with("roomtkt1"),
            "an out-of-band ticket is emitted: {}",
            summary.ticket
        );
    }

    /// R1 / AC4 at the source boundary: `agent::invite(.., id, exp)` is the same
    /// operation as `invite::invite(.., id, "agent", exp)`. Both paths, run for two
    /// distinct agent keys in one room, produce `member.invited` summaries that are
    /// field-identical except for the RNG-drawn invite id / secret and the bound
    /// key — the agent noun adds no new authorization, only surface.
    #[test]
    fn agent_invite_is_equivalent_to_room_invite_role_agent() {
        let (home, room_id) = admin_home_with_room();

        let via_agent = super::invite(home.path(), &room_id, AGENT_HEX, None).unwrap();
        // The delegated path, invoked directly for a different key in the same room.
        let via_room =
            crate::invite::invite(home.path(), &room_id, AGENT_HEX_B, "agent", None).unwrap();

        assert_eq!(via_agent.role, "agent");
        assert_eq!(
            via_agent.role, via_room.role,
            "both paths mint the identical role"
        );
        assert_eq!(via_agent.room_id, via_room.room_id, "same room scope");
        assert_eq!(via_agent.invitee_key.to_string(), AGENT_HEX);
        assert_eq!(via_room.invitee_key.to_string(), AGENT_HEX_B);
        assert_ne!(
            via_agent.invite_id, via_room.invite_id,
            "each invite draws its own RNG invite id"
        );
        assert_ne!(
            via_agent.ticket, via_room.ticket,
            "each ticket carries its own capability secret"
        );
    }

    /// The self-invite guard is delegated unchanged: inviting the caller's own
    /// identity as an agent fails before any IO, with an actionable message.
    #[test]
    fn agent_invite_self_invite_is_rejected() {
        let (home, room_id) = admin_home_with_room();
        let own_id = identity::Profile::load(home.path()).unwrap().identity_id;

        // `InviteSummary` is not `Debug`, so drop the Ok value via `.err()` rather
        // than `.unwrap_err()` before asserting on the error text.
        let err = super::invite(home.path(), &room_id, &own_id, None)
            .err()
            .expect("self-invite must be rejected");
        assert!(
            err.to_string().contains("yourself"),
            "self-invite must be rejected with a 'yourself' hint: {err}"
        );
    }

    /// The `--expires` grammar is validated by the delegated path before any IO:
    /// a malformed duration is rejected without minting an invite.
    #[test]
    fn agent_invite_bad_expires_is_rejected() {
        let (home, room_id) = admin_home_with_room();
        for bad in ["5x", "0h", "12", "h"] {
            assert!(
                super::invite(home.path(), &room_id, AGENT_HEX, Some(bad)).is_err(),
                "malformed --expires {bad:?} must be rejected"
            );
        }
    }

    // ── L3: pure pre-IO field validators (spec IR-0208 §11 L3) ──────────────────
    // These exercise the CLI-side friendly checks in isolation (no IO): they mirror
    // the strict `parse_agent_status` D1 bounds so a bad invocation fails fast with
    // an actionable message and exit 2, before any store is touched.

    use iroh_rooms_core::event::constants::{
        MAX_ARTIFACT_REFS, MAX_STATUS_LABEL_BYTES, MAX_STATUS_MESSAGE_BYTES, SHORT_ID_LEN,
    };

    #[test]
    fn validate_status_accepts_normal_and_at_cap_labels() {
        super::validate_status("running_tests").expect("a normal label is accepted");
        super::validate_status(&"a".repeat(MAX_STATUS_LABEL_BYTES))
            .expect("a label exactly at the cap is accepted");
    }

    #[test]
    fn validate_status_rejects_empty_over_cap_and_control() {
        assert!(super::validate_status("").is_err(), "empty label rejected");
        assert!(
            super::validate_status(&"a".repeat(MAX_STATUS_LABEL_BYTES + 1)).is_err(),
            "over-cap label rejected"
        );
        assert!(
            super::validate_status("run\nning").is_err(),
            "control char (newline) rejected"
        );
        assert!(
            super::validate_status("bell\u{0007}").is_err(),
            "control char (BEL) rejected"
        );
    }

    #[test]
    fn validate_message_accepts_none_newlines_unicode_and_at_cap() {
        super::validate_message(None).expect("absent message is accepted");
        super::validate_message(Some("line one\nline two — café ☕"))
            .expect("newlines and unicode are allowed in a message");
        super::validate_message(Some(&"m".repeat(MAX_STATUS_MESSAGE_BYTES)))
            .expect("a message exactly at the cap is accepted");
    }

    #[test]
    fn validate_message_rejects_over_cap() {
        assert!(
            super::validate_message(Some(&"m".repeat(MAX_STATUS_MESSAGE_BYTES + 1))).is_err(),
            "over-cap message rejected"
        );
    }

    #[test]
    fn parse_artifacts_accepts_handle_and_bare_hex_as_same_bytes() {
        // `file_<hex>` and bare 32-hex parse to the identical 16 bytes.
        let hex = "0123456789abcdef0123456789abcdef";
        let via_handle = super::parse_artifacts(&[format!("file_{hex}")]).unwrap();
        let via_bare = super::parse_artifacts(&[hex.to_owned()]).unwrap();
        assert_eq!(via_handle, via_bare, "both handle forms parse identically");
        assert_eq!(via_handle.len(), 1);
        let expected: [u8; SHORT_ID_LEN] = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(via_handle[0], expected);
    }

    #[test]
    fn parse_artifacts_deduplicates_order_preserving() {
        let a = "11".repeat(16);
        let b = "22".repeat(16);
        // a, b, a(as handle) → deduped to [a, b] preserving first-seen order.
        let out = super::parse_artifacts(&[a.clone(), b.clone(), format!("file_{a}")]).unwrap();
        assert_eq!(out.len(), 2, "a repeated artifact is deduplicated");
        assert_eq!(out[0], [0x11u8; SHORT_ID_LEN], "first-seen order preserved");
        assert_eq!(out[1], [0x22u8; SHORT_ID_LEN]);
    }

    #[test]
    fn parse_artifacts_rejects_bad_handle() {
        for bad in ["not-hex", "file_zz", "abc", &"aa".repeat(15)] {
            assert!(
                super::parse_artifacts(&[bad.to_owned()]).is_err(),
                "malformed artifact handle {bad:?} rejected"
            );
        }
    }

    #[test]
    fn parse_artifacts_rejects_over_cap_count() {
        // MAX_ARTIFACT_REFS + 1 distinct ids must be rejected.
        let ids: Vec<String> = (0..=MAX_ARTIFACT_REFS)
            .map(|i| format!("{i:02x}").repeat(16))
            .collect();
        assert!(
            super::parse_artifacts(&ids).is_err(),
            "more than MAX_ARTIFACT_REFS artifacts rejected"
        );
    }

    // ── L4: orchestration over `message::send_agent_status` (spec IR-0208 §11 L4)
    // Offline, temp home, no peers → the local-insert path. Drives the real
    // `super::status` → `message::send_agent_status` and reopens the store to prove
    // the persisted event's shape and the exact `ErrorCode` on the reject paths (the
    // assert_cmd stderr substrings in `tests/agent_cli.rs` cannot pin a code).

    use iroh_rooms_core::event::content::{Content, EventType};
    use iroh_rooms_core::event::signed::SignedEvent;
    use iroh_rooms_core::store::EventStore;
    use std::path::Path;
    use std::time::Duration;

    use crate::error::{code_of, ErrorCode};

    /// Reopen the store and return every persisted `agent.status` as its decoded
    /// `SignedEvent`, newest-first (the offline tail order).
    fn persisted_agent_statuses(home: &Path, room_id: &RoomId) -> Vec<SignedEvent> {
        let store = EventStore::open(&home.join("rooms.db")).expect("reopen store");
        store
            .room_tail(room_id, 1000)
            .expect("room_tail")
            .into_iter()
            .filter(|se| se.event_type == EventType::AgentStatus)
            .map(|se| SignedEvent::decode(&se.wire.signed).expect("agent.status decodes"))
            .collect()
    }

    #[tokio::test]
    async fn agent_status_persists_signed_event_and_returns_summary() {
        // AC1 — a valid status is signed and persisted; offline, delivered=attempted=0.
        let (home, room_id) = admin_home_with_room();
        let author = identity::Profile::load(home.path()).unwrap().identity_id;

        // `Box::pin` keeps the awaited future off the stack (clippy::large_futures).
        let summary = Box::pin(super::status(
            home.path(),
            &room_id,
            "running_tests",
            Some("running the suite"),
            None,
            &[],
            &[],
            Duration::from_secs(5),
            false,
        ))
        .await
        .expect("a valid status must be authored offline");

        assert_eq!(summary.room_id, room_id);
        assert_eq!(summary.sender_id.to_string(), author);
        assert_eq!(summary.delivered, 0, "no peers → nothing delivered");
        assert_eq!(summary.attempted, 0, "no other members → none attempted");

        let rows = persisted_agent_statuses(home.path(), &room_id);
        assert_eq!(rows.len(), 1, "exactly one agent.status persisted");
        let ev = &rows[0];
        assert_eq!(
            ev.event_id(),
            summary.event_id,
            "summary id matches the row"
        );
        assert_eq!(
            ev.sender_id.to_string(),
            author,
            "authored under the caller's identity"
        );
        assert_ne!(
            ev.sender_id.as_bytes(),
            ev.device_id.as_bytes(),
            "signed under the device key, which differs from the identity key"
        );
        let Content::AgentStatus(c) = &ev.content else {
            panic!("expected agent.status content");
        };
        assert_eq!(c.status, "running_tests");
        assert_eq!(c.message.as_deref(), Some("running the suite"));
        assert_eq!(c.progress_pct, None);
        assert_eq!(c.related_artifact_ids, None);
    }

    #[tokio::test]
    async fn agent_status_progress_and_artifacts_round_trip() {
        // Progress + two (advisory, possibly-unknown) artifact handles persist
        // verbatim as raw 16-byte ids (D5): validation does not check existence.
        let (home, room_id) = admin_home_with_room();
        let art_a = "aa".repeat(16);
        let art_b = format!("file_{}", "bb".repeat(16));

        Box::pin(super::status(
            home.path(),
            &room_id,
            "building",
            None,
            Some(40),
            &[art_a, art_b],
            &[],
            Duration::from_secs(5),
            false,
        ))
        .await
        .expect("progress + artifacts must be authored");

        let rows = persisted_agent_statuses(home.path(), &room_id);
        assert_eq!(rows.len(), 1);
        let Content::AgentStatus(c) = &rows[0].content else {
            panic!("expected agent.status content");
        };
        assert_eq!(c.progress_pct, Some(40));
        assert_eq!(
            c.related_artifact_ids,
            Some(vec![[0xaau8; SHORT_ID_LEN], [0xbbu8; SHORT_ID_LEN]]),
            "both artifact handles persist as their raw 16 bytes"
        );
    }

    #[tokio::test]
    async fn agent_status_progress_over_100_is_rejected_and_writes_nothing() {
        // AC3 — progress > 100 fails pre-IO with InvalidArgument; the store is
        // untouched (this is the only unit for the inline `progress > 100` check).
        let (home, room_id) = admin_home_with_room();

        let err = Box::pin(super::status(
            home.path(),
            &room_id,
            "running",
            None,
            Some(101),
            &[],
            &[],
            Duration::from_secs(5),
            false,
        ))
        .await
        .err()
        .expect("progress 101 must be rejected");
        assert_eq!(
            code_of(&err),
            Some(ErrorCode::InvalidArgument),
            "progress > 100 is a usage error (exit 2)"
        );
        assert!(
            persisted_agent_statuses(home.path(), &room_id).is_empty(),
            "a rejected status must write nothing"
        );
    }

    #[tokio::test]
    async fn agent_status_by_non_member_is_rejected() {
        // AC2 — a non-member (own identity, but only a copy of someone else's room
        // log) is rejected NotAMember at the is_active guard, before authoring.
        let (admin_home, room_id) = admin_home_with_room();

        // A fresh, distinct identity that is NOT a member of `room_id`.
        let outsider = TempDir::new().unwrap();
        identity::create(outsider.path(), "Mallory", false).unwrap();
        std::fs::copy(
            admin_home.path().join("rooms.db"),
            outsider.path().join("rooms.db"),
        )
        .expect("copy the room log into the outsider's home");

        let err = Box::pin(super::status(
            outsider.path(),
            &room_id,
            "running",
            None,
            None,
            &[],
            &[],
            Duration::from_secs(5),
            false,
        ))
        .await
        .err()
        .expect("a non-member status must be rejected");
        assert_eq!(
            code_of(&err),
            Some(ErrorCode::Reject(
                iroh_rooms_core::event::RejectReason::NotAMember
            )),
            "a non-member is rejected NotAMember (exit 3)"
        );
        assert!(
            persisted_agent_statuses(outsider.path(), &room_id).is_empty(),
            "a rejected non-member status must write nothing"
        );
    }
}
