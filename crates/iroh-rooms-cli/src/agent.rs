//! Agent participant orchestration: `iroh-rooms agent invite <ROOM_ID>
//! <AGENT_ID> [--expires <DURATION>]` (spec IR-0206 D1/D2).
//!
//! An agent is an ordinary principal (Spike §1): it has its own `sender_id` +
//! `device_id`, created by the shared `identity create` (#16/IR-0101), and is
//! distinguished only by `role = "agent"` at invite time. This module adds **no**
//! new authorization surface — it is a one-line delegate to the landed
//! [`crate::invite::invite`] with the role pinned to `agent`, so an agent invite is
//! byte-for-byte the same capability artifact as `room invite --role agent` (AC4).

use std::path::Path;

use anyhow::Result;
use iroh_rooms_core::event::ids::RoomId;

use crate::invite::{self, InviteSummary};

/// The invited role this module always mints (the verb *is* the role; there is no
/// `--role` flag, unlike `room invite`).
const AGENT_ROLE: &str = "agent";

/// Mint a key-bound, agent-role invite ticket for `agent_id_hex` in `room_id`.
///
/// Delegates to [`crate::invite::invite`] with `role` pinned to `"agent"`; no
/// logic is duplicated. Inherits every guard of the landed invite path: admin-only
/// authoring, self-invite rejection, key-binding, expiry parsing, and
/// validate-before-persist (store untouched on any pre-persist error).
///
/// # Errors
/// See [`crate::invite::invite`].
pub fn invite(
    home: &Path,
    room_id: &RoomId,
    agent_id_hex: &str,
    expires: Option<&str>,
) -> Result<InviteSummary> {
    invite::invite(home, room_id, agent_id_hex, AGENT_ROLE, expires)
}

/// Print an [`InviteSummary`] for an agent invite: reuse
/// [`crate::invite::print_invite`] verbatim for the script-friendly stdout lines,
/// then add an agent-tailored `next:` hint plus the PRD §13.3 "not implicitly
/// trusted" reminder on stderr so stdout stays parseable.
pub fn print_agent_invite(summary: &InviteSummary) {
    invite::print_invite(summary);
    eprintln!(
        "note: the agent is a first-class participant but is not implicitly trusted — it can \
         only access this room once it redeems the ticket via `iroh-rooms room join <ticket>`"
    );
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{invite, AGENT_ROLE};

    /// A fixed 64-hex agent identity id (32 raw bytes). `IdentityKey::from_bytes`
    /// does not check curve-point membership, so any well-formed 32-byte hex works
    /// as an invitee; this value differs from any CSPRNG-generated admin key.
    const AGENT_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";

    /// The role constant this module mints is exactly the on-wire `agent` role.
    #[test]
    fn agent_role_constant_is_agent() {
        assert_eq!(AGENT_ROLE, "agent");
    }

    /// Stand up an admin home (identity + room) and return `(home, room_id)`.
    fn admin_home() -> (TempDir, iroh_rooms_core::event::ids::RoomId) {
        let home = TempDir::new().unwrap();
        crate::identity::create(home.path(), "Alice", false).expect("identity create");
        let summary = crate::room::create(home.path(), "Test Room").expect("room create");
        (home, summary.room_id)
    }

    /// The wrapper pins the invited role to `agent` and forwards the positional
    /// `<AGENT_ID>` verbatim, with no expiry when none is passed (the core contract
    /// of `agent invite`, distinct from `room invite`'s `--role`/`--invitee`).
    #[test]
    fn invite_pins_role_to_agent_and_forwards_invitee() {
        let (home, room_id) = admin_home();

        let summary = invite(home.path(), &room_id, AGENT_HEX, None).expect("agent invite");

        assert_eq!(summary.role, "agent", "the verb must pin role to agent");
        assert_eq!(
            summary.invitee_key.to_string(),
            AGENT_HEX,
            "the positional <AGENT_ID> must be bound into the invite verbatim"
        );
        assert!(
            summary.expires_at.is_none() && summary.expires_human.is_none(),
            "no --expires must mint a non-expiring invite"
        );
        assert!(
            summary.ticket.starts_with("roomtkt1"),
            "the emitted ticket must carry the canonical roomtkt1 HRP"
        );
    }

    /// `--expires` is forwarded to the landed invite path (parity with `room invite`).
    #[test]
    fn invite_forwards_expires_duration() {
        let (home, room_id) = admin_home();

        let summary =
            invite(home.path(), &room_id, AGENT_HEX, Some("24h")).expect("agent invite --expires");

        assert!(
            summary.expires_at.is_some(),
            "an absolute expiry must be computed when --expires is given"
        );
        assert_eq!(
            summary.expires_human.as_deref(),
            Some("24h"),
            "the raw --expires string must be echoed for display"
        );
    }

    /// AC2: the persisted `member.invited` folds to `role=agent`, `status=invited`
    /// for the agent — proving the on-log role, not just the returned summary.
    #[test]
    fn invite_persists_agent_role_visible_in_members() {
        let (home, room_id) = admin_home();
        invite(home.path(), &room_id, AGENT_HEX, None).expect("agent invite");

        let view = crate::room::members(home.path(), &room_id).expect("members");
        let row = view
            .members
            .iter()
            .find(|m| m.identity_id == AGENT_HEX)
            .expect("the invited agent must appear in the membership view");

        assert_eq!(row.role, "agent", "the folded role must be agent (AC2)");
        assert_eq!(
            row.status, "invited",
            "a pre-join agent reads status=invited"
        );
        assert!(!row.is_admin, "an agent is never the room admin");
    }

    /// A self-invite (inviting the caller's own identity as an agent) is rejected by
    /// the inherited guard before any event is persisted.
    #[test]
    fn invite_self_invite_is_rejected() {
        let home = TempDir::new().unwrap();
        crate::identity::create(home.path(), "Alice", false).expect("identity create");
        let summary = crate::room::create(home.path(), "Test Room").expect("room create");

        // `InviteSummary` is not `Debug`, so use let-else instead of `expect_err`.
        let Err(err) = invite(
            home.path(),
            &summary.room_id,
            &summary.admin_identity_id,
            None,
        ) else {
            panic!("inviting yourself as an agent must fail")
        };
        assert!(
            err.to_string().to_lowercase().contains("yourself")
                || err.to_string().to_lowercase().contains("self"),
            "self-invite error must be actionable; got: {err}"
        );
    }

    /// A bad `--expires` is rejected before any IO: the pre-persist gate leaves the
    /// membership at exactly the admin (no stray invite event).
    #[test]
    fn invite_bad_expires_errs_before_persist() {
        let (home, room_id) = admin_home();

        assert!(
            invite(home.path(), &room_id, AGENT_HEX, Some("5x")).is_err(),
            "a malformed --expires must be rejected"
        );

        let view = crate::room::members(home.path(), &room_id).expect("members");
        assert_eq!(
            view.members.len(),
            1,
            "a rejected invite must persist nothing — only the admin remains"
        );
    }

    /// A malformed `<AGENT_ID>` (non-hex) is rejected before any IO.
    #[test]
    fn invite_bad_agent_id_errs() {
        let (home, room_id) = admin_home();
        let non_hex = "zz".repeat(32);

        assert!(
            invite(home.path(), &room_id, &non_hex, None).is_err(),
            "a non-hex <AGENT_ID> must be rejected"
        );
    }
}
