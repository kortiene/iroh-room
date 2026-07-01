//! Shared presentation helpers for the offline and online room read commands
//! (spec IR-0106 D5/D6).
//!
//! This module holds the small, pure projections that `room members`,
//! `room tail --offline`, and the online `room members --status` all render, so
//! the three surfaces never diverge:
//!
//! * [`MemberDisplayState`] — the display-only `active | invited | removed | left`
//!   refinement of the fold's `Status` (D5). `left` and `removed` are the **same**
//!   zero-capability security state (`Status::Removed`); the distinction is derived
//!   from the terminal departure event in the log, never from the security lattice.
//! * [`departure_sets`] — the per-subject `member.left` / `member.removed` id sets
//!   that back that refinement, read once per command from the store.
//! * [`short_id`] / [`iso8601_utc`] / [`display_names`] — the shared attribution
//!   helpers (short sender id, advisory-timestamp rendering, joined-name lookup).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::membership::Status;
use iroh_rooms_core::store::EventStore;

/// The presentational membership state of a member row (spec D5):
/// `active | invited | removed | left`.
///
/// `Left` and `Removed` collapse to the same fold `Status::Removed` (a
/// zero-capability departure); the distinction is **display-only** and derived
/// from the log, so a reader can tell a voluntary self-leave from an admin removal
/// without any change to the security lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberDisplayState {
    /// A live, joined member.
    Active,
    /// Invited but not yet joined (no device bound).
    Invited,
    /// Admin-removed (a `member.removed` targets the subject).
    Removed,
    /// Voluntarily departed (a `member.left` by the subject, no removal).
    Left,
}

impl MemberDisplayState {
    /// The stable lowercase presentation string for text and JSON output.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Invited => "invited",
            Self::Removed => "removed",
            Self::Left => "left",
        }
    }
}

/// Derive the [`MemberDisplayState`] from the folded `Status` plus, for a departed
/// subject, the log-derived departure sets (spec D5).
///
/// When the fold reports `Status::Removed`, disambiguate: a `member.removed`
/// targeting the subject ⇒ [`MemberDisplayState::Removed`] (an admin action is the
/// authoritative statement and **dominates** a concurrent self-leave); else a
/// `member.left` by the subject ⇒ [`MemberDisplayState::Left`]; else `Removed` as
/// a safe fallback (the fold says Removed even if the terminal event is not
/// classifiable here).
pub(crate) fn member_display_state(
    status: Status,
    subject: &IdentityKey,
    removed_ids: &BTreeSet<IdentityKey>,
    left_ids: &BTreeSet<IdentityKey>,
) -> MemberDisplayState {
    match status {
        Status::Active => MemberDisplayState::Active,
        Status::Invited => MemberDisplayState::Invited,
        Status::Removed => {
            if removed_ids.contains(subject) {
                MemberDisplayState::Removed
            } else if left_ids.contains(subject) {
                MemberDisplayState::Left
            } else {
                MemberDisplayState::Removed
            }
        }
    }
}

/// Read the per-subject departure id sets for a room from the store: the
/// `member.removed` subjects and the `member.left` subjects (spec D5 / Step 2).
///
/// Both are small (the membership sub-DAG only). A row that fails to decode is
/// skipped rather than aborting the whole read — the store holds only validated
/// events, so this is defensive, not expected.
///
/// # Errors
/// Fails only if the store cannot be queried.
pub(crate) fn departure_sets(
    store: &EventStore,
    room_id: &RoomId,
) -> Result<(BTreeSet<IdentityKey>, BTreeSet<IdentityKey>)> {
    let mut removed_ids = BTreeSet::new();
    for se in store
        .by_type(room_id, EventType::MemberRemoved)
        .with_context(|| format!("could not read member.removed events for room {room_id}"))?
    {
        if let Ok(ev) = SignedEvent::decode(&se.wire.signed) {
            if let Content::MemberRemoved(c) = ev.content {
                removed_ids.insert(c.member_id);
            }
        }
    }

    let mut left_ids = BTreeSet::new();
    for se in store
        .by_type(room_id, EventType::MemberLeft)
        .with_context(|| format!("could not read member.left events for room {room_id}"))?
    {
        if let Ok(ev) = SignedEvent::decode(&se.wire.signed) {
            if let Content::MemberLeft(c) = ev.content {
                left_ids.insert(c.member_id);
            }
        }
    }

    Ok((removed_ids, left_ids))
}

/// Build an identity → display-name map from the room's local `member.joined`
/// events (spec IR-0105 D10). A sender absent from the map falls back to a
/// [`short_id`] on display.
///
/// # Errors
/// Fails only if the store cannot be queried.
pub(crate) fn display_names(
    store: &EventStore,
    room_id: &RoomId,
) -> Result<BTreeMap<IdentityKey, String>> {
    let joined = store
        .by_type(room_id, EventType::MemberJoined)
        .with_context(|| format!("could not read member.joined events for room {room_id}"))?;
    let mut names = BTreeMap::new();
    for se in joined {
        let Ok(ev) = SignedEvent::decode(&se.wire.signed) else {
            continue;
        };
        if let Content::MemberJoined(c) = ev.content {
            if let Some(name) = c.display_name {
                names.insert(ev.sender_id, name);
            }
        }
    }
    Ok(names)
}

/// A short, human-friendly id: the first 8 hex chars of an identity key.
pub(crate) fn short_id(id: &IdentityKey) -> String {
    let hex = id.to_string();
    hex.get(..8).unwrap_or(&hex).to_owned()
}

/// Render a ms-since-epoch instant as an ISO-8601 UTC string
/// (`YYYY-MM-DDThh:mm:ssZ`) for the advisory `created_at` display column
/// (spec IR-0105 D10). No `chrono` dependency.
pub(crate) fn iso8601_utc(ms: u64) -> String {
    let secs = ms / 1_000;
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Convert days since the Unix epoch into a `(year, month, day)` civil date
/// (Howard Hinnant's proleptic-Gregorian algorithm; UTC, no leap seconds).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::{iso8601_utc, member_display_state, short_id, MemberDisplayState};
    use iroh_rooms_core::event::keys::IdentityKey;
    use iroh_rooms_core::membership::Status;
    use std::collections::BTreeSet;

    fn key(b: u8) -> IdentityKey {
        IdentityKey::from_bytes([b; 32])
    }

    // ── short_id ──────────────────────────────────────────────────────────────

    #[test]
    fn short_id_is_first_8_hex() {
        assert_eq!(short_id(&key(0xab)), "abababab");
    }

    // ── iso8601_utc ───────────────────────────────────────────────────────────

    #[test]
    fn iso8601_utc_unix_epoch_is_midnight() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_utc_known_timestamp() {
        // 1_750_000_000_000 ms = 1_750_000_000 s = 2025-06-15T15:06:40Z.
        assert_eq!(iso8601_utc(1_750_000_000_000), "2025-06-15T15:06:40Z");
    }

    #[test]
    fn iso8601_utc_year_2000_boundary() {
        // 946_684_800_000 ms = exactly 2000-01-01T00:00:00Z.
        assert_eq!(iso8601_utc(946_684_800_000), "2000-01-01T00:00:00Z");
    }

    // ── member_display_state (D5) ─────────────────────────────────────────────

    #[test]
    fn active_and_invited_pass_through() {
        let empty = BTreeSet::new();
        assert_eq!(
            member_display_state(Status::Active, &key(1), &empty, &empty),
            MemberDisplayState::Active
        );
        assert_eq!(
            member_display_state(Status::Invited, &key(1), &empty, &empty),
            MemberDisplayState::Invited
        );
    }

    #[test]
    fn removed_subject_reads_removed() {
        let removed: BTreeSet<_> = [key(2)].into_iter().collect();
        let left = BTreeSet::new();
        assert_eq!(
            member_display_state(Status::Removed, &key(2), &removed, &left),
            MemberDisplayState::Removed
        );
    }

    #[test]
    fn left_subject_reads_left() {
        let removed = BTreeSet::new();
        let left: BTreeSet<_> = [key(3)].into_iter().collect();
        assert_eq!(
            member_display_state(Status::Removed, &key(3), &removed, &left),
            MemberDisplayState::Left
        );
    }

    #[test]
    fn admin_removal_dominates_self_leave() {
        // A subject that both left and was removed reads `removed` (D5 dominance).
        let removed: BTreeSet<_> = [key(4)].into_iter().collect();
        let left: BTreeSet<_> = [key(4)].into_iter().collect();
        assert_eq!(
            member_display_state(Status::Removed, &key(4), &removed, &left),
            MemberDisplayState::Removed
        );
    }

    #[test]
    fn removed_status_with_no_terminal_event_falls_back_to_removed() {
        let empty = BTreeSet::new();
        assert_eq!(
            member_display_state(Status::Removed, &key(5), &empty, &empty),
            MemberDisplayState::Removed
        );
    }

    #[test]
    fn as_str_covers_all_variants() {
        assert_eq!(MemberDisplayState::Active.as_str(), "active");
        assert_eq!(MemberDisplayState::Invited.as_str(), "invited");
        assert_eq!(MemberDisplayState::Removed.as_str(), "removed");
        assert_eq!(MemberDisplayState::Left.as_str(), "left");
    }
}
