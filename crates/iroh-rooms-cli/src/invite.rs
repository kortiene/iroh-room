//! Key-bound invite minting: the orchestration behind
//! `iroh-rooms room invite <ROOM_ID> --invitee <IDENTITY_ID> [--role <ROLE>]
//! [--expires <DURATION>]` (spec IR-0103 D2/D5/D7/D10).
//!
//! This is a thin glue layer over landed primitives, the sibling of
//! [`crate::room::create`]. It loads the local signing secrets (#16), confirms the
//! caller is the room's single immutable admin by folding the persisted log (#12),
//! draws a fresh `invite_id` + capability **secret** from the OS CSPRNG, computes
//! the `capability_hash`, assembles + signs an admin `member.invited` through the
//! pure core builder, **self-validates** it (statelessly **and** through the fold,
//! so we never persist an event peers would reject), persists the verbatim event,
//! and finally emits the out-of-band [`RoomInviteTicket`] carrying the secret.
//!
//! The capability secret lives in a [`Zeroizing`] buffer from the CSPRNG draw
//! until it is rendered into the ticket string; it is **never** written to the
//! event log (only its hash is) and never appears in any other output (AC3).

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms_core::event::constants::{MAX_PREV_EVENTS, SHORT_ID_LEN};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::{build_member_invited, capability_hash};
use iroh_rooms_core::membership::{Ingest, RoomMembership};
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::ticket::RoomInviteTicket;
use zeroize::Zeroizing;

use crate::error::{CodedResultExt, ErrorCode};
use crate::{clock, identity};

/// The single event-store database file under the data-directory home (spec D3).
const DB_FILE: &str = "rooms.db";
/// Roles the CLI lets an admin issue. `admin` is rejected (single immutable admin,
/// spec D8); the on-wire enum still permits it, so the CLI is the policy gate.
const INVITABLE_ROLES: &[&str] = &["member", "agent"];

/// The result of a successful `room invite`, for the caller to present.
pub struct InviteSummary {
    /// The fresh 16-byte invite handle (hex on display).
    pub invite_id: [u8; SHORT_ID_LEN],
    /// The room the invite is scoped to.
    pub room_id: RoomId,
    /// The identity key the invite is bound to (AC2).
    pub invitee_key: IdentityKey,
    /// The invited role (`member` | `agent`).
    pub role: String,
    /// The absolute expiry (ms epoch), or `None` for no expiry.
    pub expires_at: Option<u64>,
    /// The raw `--expires` duration string, echoed for the human-readable display.
    pub expires_human: Option<String>,
    /// The copy-pasteable out-of-band ticket token (carries the secret).
    pub ticket: String,
}

/// Mint a key-bound invite ticket for `invitee_hex` in `room_id`.
///
/// Only the room's single immutable admin may issue an invite (AC1): the caller's
/// identity must equal the folded admin, checked up front *and* re-proved by
/// folding the freshly built event before it is persisted.
///
/// # Errors
/// Fails — leaving the store untouched on every pre-persist path — if `role`,
/// `invitee`, or `expires` is invalid (validated before any IO), if the invitee is
/// the caller's own identity (self-invite), if no local identity exists, if no room
/// with this id is stored, if the caller is not the room admin, if the OS CSPRNG is
/// unavailable, or — as an internal-bug guard — if the freshly built event fails
/// stateless validation or is not accepted by the fold (in which case it is **not**
/// persisted).
#[allow(clippy::too_many_lines)] // one linear orchestration; splitting hurts readability
pub fn invite(
    home: &Path,
    room_id: &RoomId,
    invitee_hex: &str,
    role: &str,
    expires: Option<&str>,
) -> Result<InviteSummary> {
    // ---- Pre-IO argument validation (a bad invocation writes nothing). ----
    let role = validate_role(role).coded(ErrorCode::InvalidArgument)?;
    let invitee_key = parse_invitee(invitee_hex)?;

    // Load the signing secrets (also re-checks them against the public profile).
    let secret = identity::SecretKeys::load(home)?;
    let admin_identity = secret.identity.identity_key();

    // Self-invite is meaningless (the admin is already a member). Caught before
    // opening the store, since the admin can only be the caller's own identity.
    if invitee_key == admin_identity {
        bail!("cannot invite yourself: --invitee is this node's own identity ({admin_identity})");
    }

    // ---- Fold the persisted log: confirm the room exists and the caller is admin. ----
    let db_path = home.join(DB_FILE);
    let mut store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;

    let ids = store
        .room_event_ids(room_id)
        .with_context(|| format!("could not read events for room {room_id}"))?;
    if ids.is_empty() {
        crate::bail_coded!(
            crate::error::ErrorCode::RoomNotFound,
            "no room {} in {}; run `iroh-rooms room create` first",
            room_id,
            home.display()
        );
    }

    let ctx = ValidationContext::for_room(*room_id);
    let mut validated = Vec::with_capacity(ids.len());
    for id in &ids {
        let stored = store
            .get(id)
            .with_context(|| format!("could not read stored event {id}"))?
            .ok_or_else(|| anyhow!("stored event {id} vanished mid-read"))?;
        let event = validate_wire_bytes(&stored.wire.to_bytes(), &ctx).map_err(|reason| {
            anyhow!("stored event {id} failed re-validation ({})", reason.code())
        })?;
        validated.push(event);
    }

    let mut membership = RoomMembership::from_events(*room_id, validated);
    let snapshot = membership.snapshot();

    // AC1 (friendly up-front gate): only the admin can invite.
    if snapshot.admin() != Some(&admin_identity) {
        bail!(
            "only the room admin can issue invites for {room_id} (this identity is {admin_identity})"
        );
    }

    // Re-inviting is legitimate after removal (sticky departure makes a stale
    // invite inert), so an already-active invitee is a warning, not an error (D9).
    if snapshot.is_active(&invitee_key) {
        eprintln!(
            "warning: {invitee_key} is already an active member of this room; \
             issuing a fresh invite anyway"
        );
    }

    // ---- prev_events = current room heads, bounded per §6 (D6). ----
    let mut heads = store
        .heads(room_id)
        .with_context(|| format!("could not read DAG heads for room {room_id}"))?;
    if heads.len() > MAX_PREV_EVENTS {
        // `heads` is already ascending by event_id; cite the 20 lowest-id heads
        // deterministically. Uncited heads remain concurrent siblings the sync
        // layer reconciles — never reached in the single-admin MVP.
        eprintln!(
            "note: room has {} heads (> {MAX_PREV_EVENTS}); citing the {MAX_PREV_EVENTS} \
             lowest-id heads",
            heads.len()
        );
        heads.truncate(MAX_PREV_EVENTS);
    }

    // ---- The only non-determinism: a clock read and two CSPRNG draws. ----
    let created_at = clock::now_ms();

    let mut invite_id = [0u8; SHORT_ID_LEN];
    getrandom::fill(&mut invite_id)
        .map_err(|err| anyhow!("OS CSPRNG (getrandom) unavailable: {err}"))?;
    // The one secret-bearing buffer; wiped on drop, only ever copied into the ticket.
    let mut secret_bytes = Zeroizing::new([0u8; SHORT_ID_LEN]);
    getrandom::fill(secret_bytes.as_mut_slice())
        .map_err(|err| anyhow!("OS CSPRNG (getrandom) unavailable: {err}"))?;

    let cap_hash = capability_hash(room_id, &invite_id, &secret_bytes);
    let expires_at = expires
        .map(|spec| parse_expires(spec, created_at))
        .transpose()
        .coded(ErrorCode::InvalidArgument)?;

    // ---- Build, self-validate, fold-check, then persist. ----
    let wire = build_member_invited(
        &secret.identity,
        &secret.device,
        room_id,
        &invite_id,
        &cap_hash,
        role,
        &invitee_key,
        expires_at,
        None,
        &heads,
        created_at,
    );

    // Belt-and-suspenders self-checks before persisting (D5 layer 2): the freshly
    // built event MUST pass the stateless pipeline AND be accepted by the fold (the
    // exact admin-signer + membership-device-binding code peers run). A failure is
    // an internal bug — surfaced as an error, never a silent persist.
    let validated_new = validate_wire_bytes(&wire.to_bytes(), &ctx)
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built member.invited failed validation ({})",
                reason.code()
            )
        })
        .coded(ErrorCode::Internal)?;
    match membership.ingest(validated_new.clone()) {
        Ingest::Accepted { .. } => {}
        Ingest::Rejected { reason, .. } => crate::bail_coded!(
            ErrorCode::Internal,
            "internal error: freshly built member.invited was rejected by the fold ({})",
            reason.code()
        ),
        Ingest::Buffered { .. } => {
            crate::bail_coded!(
                ErrorCode::Internal,
                "internal error: freshly built member.invited is causally incomplete"
            )
        }
    }

    store
        .insert(&validated_new)
        .with_context(|| format!("could not persist invite to {}", db_path.display()))?;

    // ---- Assemble the out-of-band ticket (the sole secret carrier). ----
    let ticket = RoomInviteTicket {
        room_id: *room_id,
        invite_id,
        capability_secret: *secret_bytes,
        invitee_key,
        role: role.to_owned(),
        expires_at,
        inviter_identity: admin_identity,
        discovery: vec![secret.device.device_key()],
    };
    let ticket_string = ticket.to_string();

    Ok(InviteSummary {
        invite_id,
        room_id: *room_id,
        invitee_key,
        role: role.to_owned(),
        expires_at,
        expires_human: expires.map(|s| s.trim().to_owned()),
        ticket: ticket_string,
    })
}

/// Print an [`InviteSummary`] as labeled, script-friendly lines, then the ticket on
/// its own line, then a password-grade warning (spec D10 / AC5). The secret appears
/// **only** inside the ticket token, never on its own line.
pub fn print_invite(summary: &InviteSummary) {
    println!("invite_id: {}", hex::encode(summary.invite_id));
    println!("room: {}", summary.room_id);
    println!("invitee: {}", summary.invitee_key);
    println!("role: {}", summary.role);
    match summary.expires_at {
        Some(ms) => {
            let iso = iso8601_utc(ms);
            match &summary.expires_human {
                Some(human) => println!("expires: {iso} (in {human})"),
                None => println!("expires: {iso}"),
            }
        }
        None => println!("expires: never"),
    }
    println!("ticket:");
    println!("  {}", summary.ticket);
    println!(
        "warning: this ticket carries a secret — share it over a private channel and treat it \
         like a password."
    );
    println!("next: the invitee runs `iroh-rooms room join <ticket>`");
}

/// Validate the `--role` flag (spec D8). `admin` is rejected up front; the on-wire
/// enum still permits it, but MVP has a single immutable admin so an admin-role
/// invite is a footgun.
fn validate_role(role: &str) -> Result<&str> {
    if role == "admin" {
        bail!("admin invites are not supported in MVP (single immutable admin); use --role member or --role agent");
    }
    if INVITABLE_ROLES.contains(&role) {
        Ok(role)
    } else {
        bail!("unknown role {role:?}; expected `member` or `agent`");
    }
}

/// Parse and validate the `--invitee` identity id (spec D9): 64-char lowercase hex.
fn parse_invitee(hex: &str) -> Result<IdentityKey> {
    hex.parse().map_err(|err| {
        anyhow!("invalid --invitee identity id (expected 64-char lowercase hex): {err}")
    })
}

/// Parse a compact `--expires` duration (`<int>{s|m|h|d}`) into an absolute
/// `expires_at` anchored at `created_at` (spec D7). Rejects empty, zero, unsuffixed,
/// non-numeric, and overflowing values with an actionable error, before any IO.
fn parse_expires(spec: &str, created_at: u64) -> Result<u64> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("--expires must not be empty; use <int>{{s|m|h|d}} e.g. 24h");
    }
    // The suffix is a single ASCII unit; bail before slicing on any other char.
    let unit = spec.chars().last().expect("spec is non-empty");
    let unit_ms: u64 = match unit {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => bail!("--expires must end with s, m, h, or d (e.g. 24h); got {spec:?}"),
    };
    // `unit` is ASCII here, so `spec.len() - 1` is a valid char boundary.
    let digits = &spec[..spec.len() - 1];
    if digits.is_empty() {
        bail!("--expires must include a number before the unit (e.g. 24h); got {spec:?}");
    }
    let value: u64 = digits.parse().map_err(|_| {
        anyhow!("--expires must be a positive integer with a unit (e.g. 24h); got {spec:?}")
    })?;
    if value == 0 {
        bail!("--expires must be greater than zero (e.g. 24h); got {spec:?}");
    }
    let duration_ms = value
        .checked_mul(unit_ms)
        .ok_or_else(|| anyhow!("--expires {spec:?} is too large"))?;
    created_at
        .checked_add(duration_ms)
        .ok_or_else(|| anyhow!("--expires {spec:?} overflows the clock"))
}

/// Render a ms-since-epoch instant as an ISO-8601 UTC string (`YYYY-MM-DDThh:mm:ssZ`),
/// for the absolute half of the expiry display (spec D10). No `chrono` dependency.
fn iso8601_utc(ms: u64) -> String {
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
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, iso8601_utc, parse_expires, parse_invitee, validate_role};

    const ANCHOR: u64 = 1_750_000_000_000;

    // ── validate_role ────────────────────────────────────────────────────────

    #[test]
    fn role_member_and_agent_accepted() {
        assert_eq!(validate_role("member").unwrap(), "member");
        assert_eq!(validate_role("agent").unwrap(), "agent");
    }

    #[test]
    fn role_admin_rejected_with_actionable_message() {
        let err = validate_role("admin").unwrap_err();
        assert!(err.to_string().contains("admin invites are not supported"));
    }

    #[test]
    fn role_unknown_rejected() {
        assert!(validate_role("superuser").is_err());
    }

    #[test]
    fn role_empty_string_rejected() {
        assert!(validate_role("").is_err());
    }

    #[test]
    fn role_case_sensitive_uppercase_rejected() {
        // Role matching is case-sensitive; "Member" and "AGENT" are not valid.
        assert!(validate_role("Member").is_err());
        assert!(validate_role("AGENT").is_err());
    }

    // ── parse_expires boundaries (spec §11) ──────────────────────────────────

    #[test]
    fn expires_accepts_each_unit() {
        assert_eq!(parse_expires("1s", ANCHOR).unwrap(), ANCHOR + 1_000);
        assert_eq!(parse_expires("30m", ANCHOR).unwrap(), ANCHOR + 30 * 60_000);
        assert_eq!(
            parse_expires("24h", ANCHOR).unwrap(),
            ANCHOR + 24 * 3_600_000
        );
        assert_eq!(
            parse_expires("7d", ANCHOR).unwrap(),
            ANCHOR + 7 * 86_400_000
        );
    }

    #[test]
    fn expires_trims_surrounding_whitespace() {
        assert_eq!(
            parse_expires("  24h  ", ANCHOR).unwrap(),
            ANCHOR + 24 * 3_600_000
        );
    }

    #[test]
    fn expires_rejects_empty() {
        assert!(parse_expires("", ANCHOR).is_err());
        assert!(parse_expires("   ", ANCHOR).is_err());
    }

    #[test]
    fn expires_rejects_zero() {
        assert!(parse_expires("0h", ANCHOR).is_err());
        assert!(parse_expires("0s", ANCHOR).is_err());
    }

    #[test]
    fn expires_rejects_missing_suffix() {
        assert!(parse_expires("12", ANCHOR).is_err());
    }

    #[test]
    fn expires_rejects_unknown_suffix() {
        assert!(parse_expires("5x", ANCHOR).is_err());
    }

    #[test]
    fn expires_rejects_non_numeric() {
        assert!(parse_expires("abch", ANCHOR).is_err());
        assert!(parse_expires("-5h", ANCHOR).is_err());
        assert!(parse_expires("1.5h", ANCHOR).is_err());
    }

    #[test]
    fn expires_rejects_overflow() {
        assert!(parse_expires("99999999999999999999d", ANCHOR).is_err());
        assert!(parse_expires(&format!("{}d", u64::MAX), ANCHOR).is_err());
    }

    #[test]
    fn expires_rejects_bare_suffix() {
        assert!(parse_expires("h", ANCHOR).is_err());
    }

    // ── ISO-8601 rendering ────────────────────────────────────────────────────

    #[test]
    fn iso8601_renders_known_instants() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_000_000_000_000), "2001-09-09T01:46:40Z");
    }

    #[test]
    fn civil_from_days_epoch_is_unix_zero() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    // ── iso8601_utc edge cases ───────────────────────────────────────────────

    #[test]
    fn iso8601_renders_y2k_boundary() {
        // 2000-01-01T00:00:00Z = 946_684_800 seconds since epoch.
        assert_eq!(iso8601_utc(946_684_800_000), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_renders_leap_day_2024() {
        // 2024-02-29T00:00:00Z = 1_709_164_800 seconds since epoch.
        assert_eq!(iso8601_utc(1_709_164_800_000), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn iso8601_renders_end_of_year() {
        // 1999-12-31T23:59:59Z = 946_684_799 seconds since epoch.
        assert_eq!(iso8601_utc(946_684_799_000), "1999-12-31T23:59:59Z");
    }

    // ── parse_invitee ────────────────────────────────────────────────────────

    #[test]
    fn parse_invitee_accepts_valid_hex_key() {
        // 64 lowercase hex chars decode to 32 bytes — a well-shaped identity id.
        let key = parse_invitee(&"01".repeat(32)).expect("64-char hex must parse");
        assert_eq!(key.to_string(), "01".repeat(32));
    }

    #[test]
    fn parse_invitee_rejects_too_short() {
        // 62 hex chars → 31 bytes → wrong length.
        assert!(parse_invitee(&"01".repeat(31)).is_err());
    }

    #[test]
    fn parse_invitee_rejects_too_long() {
        // 66 hex chars → 33 bytes → wrong length.
        assert!(parse_invitee(&"01".repeat(33)).is_err());
    }

    #[test]
    fn parse_invitee_rejects_non_hex() {
        // 64 chars but 'z' is not a valid hex digit.
        assert!(parse_invitee(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn parse_invitee_rejects_empty() {
        assert!(parse_invitee("").is_err());
    }
}
