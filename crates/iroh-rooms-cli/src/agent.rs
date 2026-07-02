//! The `agent` CLI noun: `iroh-rooms agent invite <ROOM_ID> <AGENT_ID> [--expires
//! <DURATION>]` (spec IR-0206 D1).
//!
//! This is a **façade**, not new authorization: an agent is an ordinary
//! principal distinguished only by `role = "agent"` (spike §1/§3.1), and the
//! admin-only, key-bound invite path is already landed (IR-0103,
//! [`crate::invite::invite`]). `agent invite` exists only so the PRD-documented
//! `agent invite <room-id> <agent-id>` surface is a first-class command instead
//! of a `--role agent` flag buried under `room invite` — every authorization
//! decision (admin gate, capability hash, ticket codec, `member.invited`
//! builder, IR-0110 error codes) is reused verbatim.

use std::path::Path;

use anyhow::Result;
use iroh_rooms_core::event::ids::RoomId;

use crate::invite::{self, InviteSummary};

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
}
