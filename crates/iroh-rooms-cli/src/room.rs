//! Room creation and inspection: the orchestration behind
//! `iroh-rooms room create <name>` and `iroh-rooms room members <room-id>`
//! (spec IR-0102 D5/D6/D7).
//!
//! This is a thin glue layer over landed primitives. `create` loads the local
//! signing secrets (#16), assembles + signs a genesis `room.created` through the
//! pure core builder (#6), **self-validates** it through the stateless §6
//! pipeline, then idempotently persists the verbatim event into the `SQLite` store
//! (#8). `members` re-derives the room's membership by folding the persisted log
//! (#12) — there is no `rooms`/`members` table; the append-only event log is the
//! single source of truth (PRD §12, Spike §9), which is exactly why a room
//! survives a CLI restart.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms_core::event::build_room_created;
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::signed;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::membership::{Role, RoomMembership, Status};
use iroh_rooms_core::store::EventStore;

use crate::{clock, identity, paths};

/// Maximum room-name length, in UTF-8 bytes (spec D7 / OQ-5; far below the
/// `message.text.body` cap of 16384 — a room name should be short).
const MAX_ROOM_NAME_BYTES: usize = 128;
/// Length of the CSPRNG `room_nonce` that seeds the `room_id` derivation (§5).
const ROOM_NONCE_LEN: usize = 16;
/// The single event-store database file under the data-directory home (spec D3).
const DB_FILE: &str = "rooms.db";

/// The result of a successful `room create`, for the caller to present.
pub struct CreateSummary {
    /// The derived room identity (`blake3:<hex>`).
    pub room_id: RoomId,
    /// The room name as created.
    pub room_name: String,
    /// The creator's `identity_id` (the room's single immutable admin), hex.
    pub admin_identity_id: String,
}

/// One resolved membership row, formatted for presentation.
pub struct MemberRow {
    /// The member's `identity_id` (`sender_id`), lowercase hex.
    pub identity_id: String,
    /// The member's resolved role (`admin` | `member` | `agent`).
    pub role: &'static str,
    /// The member's resolved status (`active` | `invited` | `removed`).
    pub status: &'static str,
    /// Whether this member is the room's immutable admin.
    pub is_admin: bool,
}

/// The folded view of a room's membership, for `room members`.
pub struct MembersView {
    /// The room these members belong to.
    pub room_id: RoomId,
    /// The room's admin `identity_id`, or `None` if no genesis is in scope.
    pub admin_identity_id: Option<String>,
    /// Every known member in deterministic identity order.
    pub members: Vec<MemberRow>,
}

/// Create a private room: assemble, sign, self-validate, and persist a genesis
/// `room.created` event; the creator becomes the room's single immutable admin.
///
/// # Errors
/// Fails if `name` is invalid (before any IO, so nothing is written), if no local
/// identity exists, if the secrets are corrupt, if the store cannot be opened or
/// written, or — as an internal-bug guard — if the freshly built genesis fails
/// stateless validation (in which case it is **not** persisted).
pub fn create(home: &Path, name: &str) -> Result<CreateSummary> {
    // Validate the name before touching the filesystem so a bad name writes
    // nothing — not even the home directory.
    validate_room_name(name)?;

    // Load the signing secrets (also re-checks them against the public profile).
    let secret = identity::SecretKeys::load(home)?;

    // Create the 0700 home before the DB (and its WAL sidecars) appear inside it.
    paths::ensure_dir(home)?;

    // The only non-determinism in the whole flow: a fresh CSPRNG nonce and a
    // clock read. Everything downstream (room_id, binding, signing, fold) is a
    // pure function of the resulting signed bytes — the basis for restart
    // determinism (spec §9 / AC5).
    let mut room_nonce = [0u8; ROOM_NONCE_LEN];
    getrandom::fill(&mut room_nonce)
        .map_err(|err| anyhow!("OS CSPRNG (getrandom) unavailable: {err}"))?;
    let created_at = clock::now_ms();

    let sender_id = secret.identity.identity_key();
    let room_id = signed::derive_room_id(&sender_id, &room_nonce, created_at);

    let wire = build_room_created(
        &secret.identity,
        &secret.device,
        name,
        &room_nonce,
        created_at,
    );

    // Belt-and-suspenders self-check before persisting: re-derive `room_id`,
    // verify the device binding, verify the signature under `device_id`, and
    // enforce `prev_events == []`. For our own freshly built event this MUST
    // pass; a failure is an internal bug, surfaced as an error, never a silent
    // persist of a malformed genesis.
    let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
        .map_err(|reason| {
            anyhow!(
                "internal error: freshly built genesis failed validation ({})",
                reason.code()
            )
        })?;

    let db_path = home.join(DB_FILE);
    let mut store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    store
        .insert(&validated)
        .with_context(|| format!("could not persist room genesis to {}", db_path.display()))?;

    Ok(CreateSummary {
        room_id,
        room_name: name.to_owned(),
        admin_identity_id: sender_id.to_string(),
    })
}

/// Re-derive a room's membership by folding its persisted event log.
///
/// Reads every stored event for `room_id`, re-validates each through the full
/// stateless pipeline, folds them into a [`MembershipSnapshot`], and returns the
/// admin plus each member. For a freshly created room this is exactly one row:
/// the creator, `admin`, `active`.
///
/// # Errors
/// Fails if the store cannot be opened or read, if no room with this id exists in
/// the store, or if a stored event fails re-validation (on-disk corruption).
pub fn members(home: &Path, room_id: &RoomId) -> Result<MembersView> {
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;

    let ids = store
        .room_event_ids(room_id)
        .with_context(|| format!("could not read events for room {room_id}"))?;
    if ids.is_empty() {
        bail!("no room {} in {}", room_id, home.display());
    }

    // Re-validate each stored event through the full §6 pipeline before folding,
    // so a corrupt row fails loudly rather than silently skewing membership.
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

    let snapshot = RoomMembership::from_events(*room_id, validated).snapshot();
    let admin_identity_id = snapshot.admin().map(ToString::to_string);
    let members = snapshot
        .members()
        .map(|m| MemberRow {
            identity_id: m.identity.to_string(),
            role: role_str(m.role),
            status: status_str(m.status),
            is_admin: snapshot.admin() == Some(&m.identity),
        })
        .collect();

    Ok(MembersView {
        room_id: *room_id,
        admin_identity_id,
        members,
    })
}

/// Print a [`MembersView`] as labeled, script-friendly lines in deterministic
/// order (spec D6).
pub fn print_members(view: &MembersView) {
    println!("room: {}", view.room_id);
    match &view.admin_identity_id {
        Some(admin) => println!("admin: {admin}"),
        None => println!("admin: <none>"),
    }
    for m in &view.members {
        let admin_tag = if m.is_admin { " (admin)" } else { "" };
        println!(
            "member: {} role={} status={}{admin_tag}",
            m.identity_id, m.role, m.status
        );
    }
}

/// Validate a room name: 1..=128 UTF-8 bytes, no control characters (so it stays
/// clean in `members` output and CBOR content) (spec D7).
fn validate_room_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("room name must not be empty");
    }
    let len = name.len();
    if len > MAX_ROOM_NAME_BYTES {
        bail!("room name must be at most {MAX_ROOM_NAME_BYTES} bytes (got {len})");
    }
    if name.chars().any(char::is_control) {
        bail!("room name must not contain control characters (newline, tab, etc.)");
    }
    Ok(())
}

/// The presentation string for a [`Role`].
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
        Role::Agent => "agent",
    }
}

/// The presentation string for a [`Status`].
fn status_str(status: Status) -> &'static str {
    match status {
        Status::Active => "active",
        Status::Invited => "invited",
        Status::Removed => "removed",
    }
}

#[cfg(test)]
mod tests {
    use super::{create, members, role_str, status_str, validate_room_name, MAX_ROOM_NAME_BYTES};
    use crate::identity;
    use iroh_rooms_core::event::ids::RoomId;
    use iroh_rooms_core::membership::{Role, Status};
    use tempfile::TempDir;

    // ── validate_room_name ────────────────────────────────────────────────────

    #[test]
    fn rejects_empty_name() {
        assert!(validate_room_name("").is_err());
    }

    #[test]
    fn accepts_single_byte_name() {
        assert!(validate_room_name("a").is_ok());
    }

    #[test]
    fn accepts_exactly_max_bytes() {
        let max = "a".repeat(MAX_ROOM_NAME_BYTES);
        assert!(validate_room_name(&max).is_ok());
    }

    #[test]
    fn rejects_one_over_max_bytes() {
        let too_long = "a".repeat(MAX_ROOM_NAME_BYTES + 1);
        let err = validate_room_name(&too_long).unwrap_err();
        assert!(
            err.to_string().contains(&MAX_ROOM_NAME_BYTES.to_string()),
            "error must mention the byte limit: {err}"
        );
    }

    #[test]
    fn rejects_control_characters() {
        assert!(validate_room_name("Build\nRoom").is_err());
        assert!(validate_room_name("Build\tRoom").is_err());
        assert!(validate_room_name("\0").is_err());
    }

    #[test]
    fn accepts_unicode_within_byte_limit() {
        assert!(validate_room_name("Salle de réunion — café ☕").is_ok());
    }

    // ── role_str / status_str: all enum variants map to the expected string ──

    #[test]
    fn role_str_covers_all_variants() {
        assert_eq!(role_str(Role::Admin), "admin");
        assert_eq!(role_str(Role::Member), "member");
        assert_eq!(role_str(Role::Agent), "agent");
    }

    #[test]
    fn status_str_covers_all_variants() {
        assert_eq!(status_str(Status::Active), "active");
        assert_eq!(status_str(Status::Invited), "invited");
        assert_eq!(status_str(Status::Removed), "removed");
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn home_with_identity() -> TempDir {
        let dir = TempDir::new().unwrap();
        identity::create(dir.path(), "Alice", false).unwrap();
        dir
    }

    // ── create: Rust API (AC1 / AC2 / AC3 / AC4 without binary spawn) ────────

    #[test]
    fn create_room_id_is_valid_blake3_hex() {
        let home = home_with_identity();
        let summary = create(home.path(), "Build Room").unwrap();
        let s = summary.room_id.to_string();
        assert!(
            s.starts_with("blake3:"),
            "room_id must start with 'blake3:' but got: {s}"
        );
        let hex = s.strip_prefix("blake3:").unwrap();
        assert_eq!(hex.len(), 64, "room_id hex part must be 64 chars");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "room_id hex must be lowercase: {hex}"
        );
    }

    #[test]
    fn create_summary_preserves_room_name() {
        let home = home_with_identity();
        let summary = create(home.path(), "My Room").unwrap();
        assert_eq!(summary.room_name, "My Room");
    }

    #[test]
    fn create_admin_identity_id_equals_creator() {
        let home = home_with_identity();
        let profile = identity::Profile::load(home.path()).unwrap();
        let summary = create(home.path(), "Room").unwrap();
        assert_eq!(
            summary.admin_identity_id, profile.identity_id,
            "admin_identity_id must be the creator's identity key"
        );
    }

    #[test]
    fn create_writes_rooms_db() {
        let home = home_with_identity();
        assert!(
            !home.path().join("rooms.db").exists(),
            "rooms.db must not pre-exist"
        );
        create(home.path(), "Room").unwrap();
        assert!(
            home.path().join("rooms.db").exists(),
            "rooms.db must be created by create"
        );
    }

    #[test]
    fn create_without_identity_returns_actionable_error() {
        let dir = TempDir::new().unwrap();
        let result = create(dir.path(), "Room");
        let Err(err) = result else {
            panic!("expected create to fail with no identity but it succeeded")
        };
        let msg = err.to_string();
        assert!(
            msg.contains("identity create") || msg.contains("no identity"),
            "error must hint at 'identity create': {msg}"
        );
    }

    #[test]
    fn create_bad_name_writes_no_rooms_db() {
        let home = home_with_identity();
        let _ = create(home.path(), "");
        assert!(
            !home.path().join("rooms.db").exists(),
            "rooms.db must not be created when the name is invalid (pre-IO validation)"
        );
    }

    #[test]
    fn create_bad_name_with_control_char_writes_no_rooms_db() {
        let home = home_with_identity();
        let _ = create(home.path(), "Bad\nName");
        assert!(
            !home.path().join("rooms.db").exists(),
            "rooms.db must not be created when the name contains control characters"
        );
    }

    #[test]
    fn two_creates_produce_distinct_room_ids() {
        let home = home_with_identity();
        let a = create(home.path(), "Room A").unwrap();
        let b = create(home.path(), "Room B").unwrap();
        assert_ne!(
            a.room_id, b.room_id,
            "each create must produce a distinct room_id via nonce"
        );
    }

    // ── members: Rust API (AC4 / AC5 without binary spawn) ───────────────────

    #[test]
    fn members_after_create_has_one_admin_active_row() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        let view = members(home.path(), &summary.room_id).unwrap();

        assert_eq!(
            view.members.len(),
            1,
            "freshly created room has exactly one member"
        );
        let m = &view.members[0];
        assert_eq!(m.role, "admin");
        assert_eq!(m.status, "active");
        assert!(m.is_admin, "creator must be flagged is_admin");
    }

    #[test]
    fn members_admin_identity_id_matches_create_summary() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        let view = members(home.path(), &summary.room_id).unwrap();
        assert_eq!(
            view.admin_identity_id.as_deref(),
            Some(summary.admin_identity_id.as_str()),
            "admin_identity_id in MembersView must equal the CreateSummary's admin_identity_id"
        );
    }

    #[test]
    fn members_room_id_echoes_the_queried_room_id() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        let view = members(home.path(), &summary.room_id).unwrap();
        assert_eq!(
            view.room_id, summary.room_id,
            "room_id in MembersView must equal the room_id returned by create"
        );
    }

    #[test]
    fn members_member_identity_id_is_the_admin() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        let view = members(home.path(), &summary.room_id).unwrap();
        assert_eq!(
            view.members[0].identity_id, summary.admin_identity_id,
            "the single member's identity_id must be the admin"
        );
    }

    #[test]
    fn members_unknown_room_id_returns_error() {
        let home = home_with_identity();
        create(home.path(), "Some Room").unwrap();
        let unknown = RoomId::from_bytes([0xde; 32]);
        assert!(
            members(home.path(), &unknown).is_err(),
            "querying an unknown room_id must return an error"
        );
    }

    #[test]
    fn members_persists_across_second_call() {
        let home = home_with_identity();
        let summary = create(home.path(), "Persistent").unwrap();
        let view1 = members(home.path(), &summary.room_id).unwrap();
        let view2 = members(home.path(), &summary.room_id).unwrap();
        assert_eq!(view1.room_id, view2.room_id);
        assert_eq!(view1.admin_identity_id, view2.admin_identity_id);
        assert_eq!(view1.members.len(), view2.members.len());
    }
}
