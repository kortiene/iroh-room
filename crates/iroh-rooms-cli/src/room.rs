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

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms_core::event::build_room_created;
use iroh_rooms_core::event::content::Content;
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::membership::{MembershipSnapshot, Role, RoomMembership};
use iroh_rooms_core::store::EventStore;
use serde_json::{json, Map, Value};

use crate::{clock, display, identity, message, paths};

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
    /// The member's display status (`active` | `invited` | `removed` | `left`),
    /// the log-derived D5 refinement of the fold `Status` (removed vs left).
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

    // Log-derived left/removed refinement (D5): a departed member reads `left`
    // (voluntary) vs `removed` (admin action), not just the fold's `Removed`.
    let (removed_ids, left_ids) = display::departure_sets(&store, room_id)?;

    let admin_identity_id = snapshot.admin().map(ToString::to_string);
    let members = snapshot
        .members()
        .map(|m| MemberRow {
            identity_id: m.identity.to_string(),
            role: role_str(m.role),
            status: display::member_display_state(m.status, &m.identity, &removed_ids, &left_ids)
                .as_str(),
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

/// Print a [`MembersView`] as a single-line JSON object (spec D4/D6). Field names
/// are stable lowercase-snake and member order is the fold's deterministic
/// identity order; `admin` is `null` when no genesis is in scope. Mirrors
/// `identity show --json`.
///
/// # Errors
/// Fails only if JSON encoding fails (it cannot, for this value).
pub fn print_members_json(view: &MembersView) -> Result<()> {
    let members: Vec<Value> = view
        .members
        .iter()
        .map(|m| {
            json!({
                "identity_id": m.identity_id,
                "role": m.role,
                "status": m.status,
                "is_admin": m.is_admin,
            })
        })
        .collect();
    let obj = json!({
        "room": view.room_id.to_string(),
        "admin": view.admin_identity_id,
        "members": members,
    });
    let line = serde_json::to_string(&obj).context("could not encode members as JSON")?;
    println!("{line}");
    Ok(())
}

/// One row of the offline `room tail --offline --json` output (spec D6). Common
/// attribution fields are stable and lowercase-snake; type-specific fields are
/// flattened in from [`content_fields`] so structured tests can assert on `body`,
/// `room_name`, etc. directly. `None` fields are omitted.
#[derive(serde::Serialize)]
struct TailRow {
    /// The event id (`blake3:<hex>`).
    event_id: String,
    /// The stable dotted event-type name (`room.created`, `message.text`, …).
    event_type: &'static str,
    /// Derived Lamport position (always present — `room_tail` excludes NULL).
    lamport: u64,
    /// Admin-chain position, for admin-authored events only.
    #[serde(skip_serializing_if = "Option::is_none")]
    admin_seq: Option<u64>,
    /// Advisory `created_at` (ms since epoch); display-only, never orders.
    created_at: u64,
    /// `created_at` rendered as ISO-8601 UTC.
    at: String,
    /// Short sender id (first 8 hex of `sender_id`).
    from: String,
    /// The sender's display name, if a local `member.joined` named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    /// The sender's current role (`admin` | `member` | `agent` | `unknown`).
    role: &'static str,
    /// The sender's current display status (`active|invited|removed|left|unknown`).
    status: &'static str,
    /// Type-specific structured fields (flattened; see [`content_fields`]).
    #[serde(flatten)]
    content: Map<String, Value>,
}

/// Offline, deterministic timeline read of the local log (spec D1–D3, D6).
///
/// A pure local-DB projection: open the store, fold the (re-validated) log for
/// sender attribution, render the causally-complete timeline in canonical
/// `(lamport, event_id)` order, and exit. No network, no `Node`, no identity or
/// secret load, no membership requirement — the same trust posture as the landed
/// offline `room members`. Every stored event type is projected (not just
/// messages), each with a stable attribution header and a type-specific summary.
///
/// # Errors
/// Fails if the store cannot be opened, if no room with this id exists, if a
/// stored event fails re-validation (on-disk corruption, surfaced by
/// [`message::fold_room`]), or if JSON encoding fails.
pub fn tail_offline(home: &Path, room_id: &RoomId, limit: u32, json: bool) -> Result<()> {
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;

    // Re-validate + fold the whole log (this also enforces "room exists" and fails
    // loudly on a corrupt row), then read the log-derived left/removed sets and
    // joined display names — the same inputs the offline `room members` uses.
    let (_, snapshot) = message::fold_room(&store, home, room_id)?;
    let (removed_ids, left_ids) = display::departure_sets(&store, room_id)?;
    let names = display::display_names(&store, room_id)?;

    let rows = store
        .room_tail(room_id, limit)
        .with_context(|| format!("could not read the timeline for room {room_id}"))?;

    let mut json_rows: Vec<TailRow> = Vec::with_capacity(rows.len());
    for se in &rows {
        // Every row came from the already-validated set; a decode failure here is
        // on-disk corruption — surface it, never silently skip (spec §7).
        let ev = SignedEvent::decode(&se.wire.signed).map_err(|reason| {
            anyhow!(
                "stored event {} failed to decode during tail ({reason:?})",
                se.event_id
            )
        })?;

        let from = display::short_id(&ev.sender_id);
        let role = attribution_role(&snapshot, &ev.sender_id);
        let status = attribution_status(&snapshot, &ev.sender_id, &removed_ids, &left_ids);
        let display_name = names.get(&ev.sender_id).cloned();
        let at = display::iso8601_utc(ev.created_at);
        let lamport = se.lamport.unwrap_or_default();

        if json {
            json_rows.push(TailRow {
                event_id: se.event_id.to_string(),
                event_type: ev.event_type.as_str(),
                lamport,
                admin_seq: se.admin_seq,
                created_at: ev.created_at,
                at,
                from,
                display_name,
                role,
                status,
                content: content_fields(&ev.content),
            });
        } else {
            let summary = content_summary(&ev.content);
            println!(
                "event={} type={} lamport={lamport} from={from} role={role} status={status} \
                 at={at}  {summary}",
                se.event_id,
                ev.event_type.as_str(),
            );
        }
    }

    if json {
        let line =
            serde_json::to_string(&json_rows).context("could not encode the timeline as JSON")?;
        println!("{line}");
    }
    Ok(())
}

/// The sender's current role for attribution (`admin|member|agent|unknown`).
fn attribution_role(snapshot: &MembershipSnapshot, sender: &IdentityKey) -> &'static str {
    snapshot.role(sender).map_or("unknown", role_str)
}

/// The sender's current display status for attribution
/// (`active|invited|removed|left|unknown`), applying the D5 left/removed refinement.
fn attribution_status(
    snapshot: &MembershipSnapshot,
    sender: &IdentityKey,
    removed_ids: &BTreeSet<IdentityKey>,
    left_ids: &BTreeSet<IdentityKey>,
) -> &'static str {
    match snapshot.status(sender) {
        Some(status) => {
            display::member_display_state(status, sender, removed_ids, left_ids).as_str()
        }
        None => "unknown",
    }
}

/// A short, human-friendly hex handle: the first 8 hex chars of a byte slice
/// (e.g. a `pipe_id` / `file_id`), or the whole thing if shorter.
fn short_hex(bytes: &[u8]) -> String {
    let hex = hex::encode(bytes);
    hex.get(..8).unwrap_or(&hex).to_owned()
}

/// A one-line, human-facing summary of an event's content for the offline tail
/// text output (spec D2/D6). This is the free-form tail tests do **not** parse;
/// the stable `key=value` prefix precedes it.
fn content_summary(content: &Content) -> String {
    match content {
        Content::RoomCreated(c) => format!("name={:?}", c.room_name),
        Content::MemberInvited(c) => {
            let expires = c.expires_at.map_or_else(String::new, |exp| {
                format!(" expires={}", display::iso8601_utc(exp))
            });
            format!(
                "invitee={} role={}{expires}",
                display::short_id(&c.invitee_key),
                c.role
            )
        }
        Content::MemberJoined(c) => {
            let name = c
                .display_name
                .as_ref()
                .map_or_else(String::new, |n| format!(" name={n:?}"));
            format!("role={}{name}", c.role)
        }
        Content::MemberLeft(c) => c
            .reason
            .as_ref()
            .map_or_else(String::new, |r| format!("reason={r:?}")),
        Content::MemberRemoved(c) => {
            let reason = c
                .reason
                .as_ref()
                .map_or_else(String::new, |r| format!(" reason={r:?}"));
            format!(
                "subject={} by={}{reason}",
                display::short_id(&c.member_id),
                display::short_id(&c.removed_by)
            )
        }
        Content::MessageText(c) => {
            let fmt = c
                .format
                .as_ref()
                .map_or_else(String::new, |f| format!("format={f} "));
            format!("{fmt}body={}", c.body)
        }
        Content::FileShared(c) => format!(
            "name={:?} size={} hash={}",
            c.name,
            c.size_bytes,
            short_hex(c.blob_hash.as_bytes())
        ),
        Content::PipeOpened(c) => {
            let label = if c.label.is_empty() {
                String::new()
            } else {
                format!(" label={:?}", c.label)
            };
            format!("pipe={}{label}", short_hex(&c.pipe_id))
        }
        Content::PipeClosed(c) => {
            let reason = c
                .reason
                .as_ref()
                .map_or_else(String::new, |r| format!(" reason={r}"));
            format!("pipe={}{reason}", short_hex(&c.pipe_id))
        }
        Content::AgentStatus(c) => {
            let msg = c
                .message
                .as_ref()
                .map_or_else(String::new, |m| format!(" text={m:?}"));
            format!("state={}{msg}", c.status)
        }
    }
}

/// Type-specific structured JSON fields for a tail row (spec D6). Kept minimal and
/// additive: identity keys are full lowercase hex, named hashes/ids are their
/// `blake3:<hex>` form, and omit-when-empty optionals are simply absent.
fn content_fields(content: &Content) -> Map<String, Value> {
    let mut m = Map::new();
    match content {
        Content::RoomCreated(c) => {
            m.insert("room_name".into(), json!(c.room_name));
        }
        Content::MemberInvited(c) => {
            m.insert("invitee".into(), json!(c.invitee_key.to_string()));
            m.insert("invited_role".into(), json!(c.role));
            if let Some(exp) = c.expires_at {
                m.insert("expires_at".into(), json!(exp));
            }
        }
        Content::MemberJoined(c) => {
            m.insert("joined_role".into(), json!(c.role));
        }
        Content::MemberLeft(c) => {
            if let Some(r) = &c.reason {
                m.insert("reason".into(), json!(r));
            }
        }
        Content::MemberRemoved(c) => {
            m.insert("subject".into(), json!(c.member_id.to_string()));
            m.insert("removed_by".into(), json!(c.removed_by.to_string()));
            if let Some(r) = &c.reason {
                m.insert("reason".into(), json!(r));
            }
        }
        Content::MessageText(c) => {
            m.insert("body".into(), json!(c.body));
            // `format` defaults to `plain` on read when omitted (§7).
            m.insert(
                "format".into(),
                json!(c.format.as_deref().unwrap_or("plain")),
            );
            if let Some(id) = &c.in_reply_to {
                m.insert("in_reply_to".into(), json!(id.to_string()));
            }
        }
        Content::FileShared(c) => {
            m.insert("file_name".into(), json!(c.name));
            m.insert("size_bytes".into(), json!(c.size_bytes));
            m.insert("blob_hash".into(), json!(c.blob_hash.to_string()));
        }
        Content::PipeOpened(c) => {
            m.insert("pipe_id".into(), json!(hex::encode(c.pipe_id)));
            if !c.label.is_empty() {
                m.insert("label".into(), json!(c.label));
            }
        }
        Content::PipeClosed(c) => {
            m.insert("pipe_id".into(), json!(hex::encode(c.pipe_id)));
            if let Some(r) = &c.reason {
                m.insert("reason".into(), json!(r));
            }
        }
        Content::AgentStatus(c) => {
            m.insert("state".into(), json!(c.status));
            if let Some(msg) = &c.message {
                m.insert("message".into(), json!(msg));
            }
        }
    }
    m
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

#[cfg(test)]
mod tests {
    use super::{
        content_fields, content_summary, create, members, role_str, tail_offline,
        validate_room_name, MAX_ROOM_NAME_BYTES,
    };
    use crate::identity;
    use iroh_rooms_core::event::binding::DeviceBinding;
    use iroh_rooms_core::event::content::{
        AgentStatus, Content, FileShared, MemberInvited, MemberJoined, MemberLeft, MemberRemoved,
        MessageText, PipeClosed, PipeOpened, RoomCreated,
    };
    use iroh_rooms_core::event::ids::{EventId, HashRef, RoomId};
    use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, Signature};
    use iroh_rooms_core::membership::Role;
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

    // ── content_fields: structured JSON field extraction per event type ────────

    #[test]
    fn content_fields_message_text_has_body_and_default_format() {
        let content = Content::MessageText(MessageText {
            body: "hello world".to_string(),
            format: None,
            in_reply_to: None,
            mentions: None,
        });
        let fields = content_fields(&content);
        assert_eq!(
            fields["body"].as_str(),
            Some("hello world"),
            "body field must equal the message body"
        );
        assert_eq!(
            fields["format"].as_str(),
            Some("plain"),
            "format must default to 'plain' when absent"
        );
        assert!(
            !fields.contains_key("in_reply_to"),
            "in_reply_to must be absent when None"
        );
    }

    #[test]
    fn content_fields_message_text_explicit_format_is_preserved() {
        let content = Content::MessageText(MessageText {
            body: "**bold**".to_string(),
            format: Some("markdown".to_string()),
            in_reply_to: None,
            mentions: None,
        });
        let fields = content_fields(&content);
        assert_eq!(fields["format"].as_str(), Some("markdown"));
    }

    #[test]
    fn content_fields_message_text_in_reply_to_present_when_set() {
        let reply_id = EventId::from_bytes([0xab; 32]);
        let content = Content::MessageText(MessageText {
            body: "reply".to_string(),
            format: None,
            in_reply_to: Some(reply_id),
            mentions: None,
        });
        let fields = content_fields(&content);
        assert!(
            fields.contains_key("in_reply_to"),
            "in_reply_to must be present when set"
        );
    }

    #[test]
    fn content_fields_member_left_has_reason_when_set() {
        let content = Content::MemberLeft(MemberLeft {
            member_id: IdentityKey::from_bytes([0x01; 32]),
            reason: Some("moving on".to_string()),
        });
        let fields = content_fields(&content);
        assert_eq!(
            fields["reason"].as_str(),
            Some("moving on"),
            "reason must appear when set"
        );
    }

    #[test]
    fn content_fields_member_left_omits_reason_when_none() {
        let content = Content::MemberLeft(MemberLeft {
            member_id: IdentityKey::from_bytes([0x01; 32]),
            reason: None,
        });
        let fields = content_fields(&content);
        assert!(
            !fields.contains_key("reason"),
            "reason must be absent when None"
        );
    }

    #[test]
    fn content_fields_agent_status_has_state_and_message() {
        let content = Content::AgentStatus(AgentStatus {
            status: "running".to_string(),
            message: Some("all good".to_string()),
            related_artifact_ids: None,
            progress_pct: None,
        });
        let fields = content_fields(&content);
        assert_eq!(
            fields["state"].as_str(),
            Some("running"),
            "state must map the AgentStatus.status field"
        );
        assert_eq!(
            fields["message"].as_str(),
            Some("all good"),
            "message must be present when set"
        );
    }

    #[test]
    fn content_fields_agent_status_omits_message_when_none() {
        let content = Content::AgentStatus(AgentStatus {
            status: "idle".to_string(),
            message: None,
            related_artifact_ids: None,
            progress_pct: None,
        });
        let fields = content_fields(&content);
        assert_eq!(fields["state"].as_str(), Some("idle"));
        assert!(
            !fields.contains_key("message"),
            "message must be absent when None"
        );
    }

    #[test]
    fn content_fields_pipe_closed_has_lowercase_hex_pipe_id() {
        let content = Content::PipeClosed(PipeClosed {
            pipe_id: [0xab; 16],
            reason: None,
        });
        let fields = content_fields(&content);
        let pipe_id_str = fields["pipe_id"]
            .as_str()
            .expect("pipe_id must be a string");
        assert_eq!(
            pipe_id_str.len(),
            32,
            "pipe_id must be 32 hex chars (16 bytes)"
        );
        assert!(
            pipe_id_str
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "pipe_id must be lowercase hex: {pipe_id_str}"
        );
    }

    #[test]
    fn content_fields_pipe_closed_has_reason_when_set() {
        let content = Content::PipeClosed(PipeClosed {
            pipe_id: [0x00; 16],
            reason: Some("expired".to_string()),
        });
        let fields = content_fields(&content);
        assert_eq!(fields["reason"].as_str(), Some("expired"));
    }

    // ── content_summary: human-readable one-line summary per event type ────────

    #[test]
    fn content_summary_message_text_contains_body() {
        let content = Content::MessageText(MessageText {
            body: "hello tests".to_string(),
            format: None,
            in_reply_to: None,
            mentions: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("hello tests"),
            "summary for MessageText must contain the body: {summary:?}"
        );
    }

    #[test]
    fn content_summary_message_text_with_format_contains_format_label() {
        let content = Content::MessageText(MessageText {
            body: "**bold**".to_string(),
            format: Some("markdown".to_string()),
            in_reply_to: None,
            mentions: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("format=markdown"),
            "summary with explicit format must contain format=<format>: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_left_with_reason_contains_reason() {
        let content = Content::MemberLeft(MemberLeft {
            member_id: IdentityKey::from_bytes([0x01; 32]),
            reason: Some("done here".to_string()),
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("done here"),
            "summary for MemberLeft with reason must contain the reason: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_left_without_reason_is_empty() {
        let content = Content::MemberLeft(MemberLeft {
            member_id: IdentityKey::from_bytes([0x01; 32]),
            reason: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.is_empty(),
            "summary for MemberLeft without reason must be empty, got: {summary:?}"
        );
    }

    #[test]
    fn content_summary_agent_status_contains_state() {
        let content = Content::AgentStatus(AgentStatus {
            status: "running".to_string(),
            message: None,
            related_artifact_ids: None,
            progress_pct: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("state=running"),
            "summary for AgentStatus must contain state=<status>: {summary:?}"
        );
    }

    // ── tail_offline: error and success paths ─────────────────────────────────

    #[test]
    fn tail_offline_unknown_room_id_returns_actionable_error() {
        let home = home_with_identity();
        create(home.path(), "Seed Room").unwrap(); // so rooms.db exists
        let unknown = RoomId::from_bytes([0xde; 32]);
        let err = tail_offline(home.path(), &unknown, 100, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no room"),
            "error for an unknown room_id must mention 'no room': {msg}"
        );
    }

    #[test]
    fn tail_offline_fresh_room_returns_ok() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        // tail_offline prints to stdout but must return Ok for a valid room.
        tail_offline(home.path(), &summary.room_id, 100, false)
            .expect("tail_offline on a valid fresh room must return Ok");
    }

    #[test]
    fn tail_offline_json_mode_returns_ok() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        tail_offline(home.path(), &summary.room_id, 100, true)
            .expect("tail_offline in JSON mode must return Ok");
    }

    #[test]
    fn tail_offline_limit_zero_returns_ok() {
        let home = home_with_identity();
        let summary = create(home.path(), "Room").unwrap();
        // limit=0 means no events returned; must still exit Ok.
        tail_offline(home.path(), &summary.room_id, 0, false)
            .expect("tail_offline with limit=0 must return Ok");
    }

    // ── content_fields: untested variants ────────────────────────────────────

    fn dummy_binding() -> DeviceBinding {
        DeviceBinding {
            identity_key: IdentityKey::from_bytes([0x01; 32]),
            device_key: DeviceKey::from_bytes([0x02; 32]),
            sig: Signature::from_bytes([0; 64]),
        }
    }

    #[test]
    fn content_fields_room_created_has_room_name() {
        let content = Content::RoomCreated(RoomCreated {
            room_name: "Engineering HQ".to_string(),
            room_nonce: [0u8; 16],
            admins: vec![],
            device_binding: dummy_binding(),
        });
        let fields = content_fields(&content);
        assert_eq!(
            fields["room_name"].as_str(),
            Some("Engineering HQ"),
            "room_name must equal the room's creation name"
        );
    }

    #[test]
    fn content_fields_member_invited_has_invitee_and_role_no_expires() {
        let content = Content::MemberInvited(MemberInvited {
            invite_id: [0u8; 16],
            capability_hash: [0u8; 32],
            role: "member".to_string(),
            invitee_key: IdentityKey::from_bytes([0x05; 32]),
            expires_at: None,
            invitee_hint: None,
        });
        let fields = content_fields(&content);
        assert_eq!(fields["invited_role"].as_str(), Some("member"));
        assert!(
            fields["invitee"].as_str().is_some(),
            "invitee field must be present"
        );
        assert!(
            !fields.contains_key("expires_at"),
            "expires_at must be absent when None"
        );
    }

    #[test]
    fn content_fields_member_invited_has_expires_at_when_set() {
        let content = Content::MemberInvited(MemberInvited {
            invite_id: [0u8; 16],
            capability_hash: [0u8; 32],
            role: "agent".to_string(),
            invitee_key: IdentityKey::from_bytes([0x06; 32]),
            expires_at: Some(1_750_000_000_000),
            invitee_hint: None,
        });
        let fields = content_fields(&content);
        assert!(
            fields.contains_key("expires_at"),
            "expires_at must be present when set"
        );
        assert_eq!(
            fields["expires_at"].as_u64(),
            Some(1_750_000_000_000),
            "expires_at must equal the set value"
        );
    }

    #[test]
    fn content_fields_member_joined_has_joined_role() {
        let content = Content::MemberJoined(MemberJoined {
            via_invite_id: [0u8; 16],
            capability_secret: [0u8; 16],
            role: "member".to_string(),
            device_binding: dummy_binding(),
            display_name: None,
        });
        let fields = content_fields(&content);
        assert_eq!(
            fields["joined_role"].as_str(),
            Some("member"),
            "joined_role must equal the joined role"
        );
    }

    #[test]
    fn content_fields_member_removed_has_subject_and_removed_by_no_reason() {
        let content = Content::MemberRemoved(MemberRemoved {
            member_id: IdentityKey::from_bytes([0x07; 32]),
            removed_by: IdentityKey::from_bytes([0x08; 32]),
            reason: None,
            device_binding: None,
        });
        let fields = content_fields(&content);
        assert!(
            fields["subject"].as_str().is_some(),
            "subject field must be present"
        );
        assert!(
            fields["removed_by"].as_str().is_some(),
            "removed_by field must be present"
        );
        assert!(
            !fields.contains_key("reason"),
            "reason must be absent when None"
        );
    }

    #[test]
    fn content_fields_member_removed_has_reason_when_set() {
        let content = Content::MemberRemoved(MemberRemoved {
            member_id: IdentityKey::from_bytes([0x07; 32]),
            removed_by: IdentityKey::from_bytes([0x08; 32]),
            reason: Some("policy violation".to_string()),
            device_binding: None,
        });
        let fields = content_fields(&content);
        assert_eq!(fields["reason"].as_str(), Some("policy violation"));
    }

    #[test]
    fn content_fields_file_shared_has_name_size_and_blake3_hash() {
        let content = Content::FileShared(FileShared {
            file_id: [0u8; 16],
            name: "report.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size_bytes: 42_000,
            blob_hash: HashRef::from_bytes([0xbb; 32]),
            blob_format: None,
            providers: None,
        });
        let fields = content_fields(&content);
        assert_eq!(fields["file_name"].as_str(), Some("report.pdf"));
        assert_eq!(fields["size_bytes"].as_u64(), Some(42_000));
        assert!(
            fields["blob_hash"]
                .as_str()
                .is_some_and(|s| s.starts_with("blake3:")),
            "blob_hash must be a blake3: string"
        );
    }

    #[test]
    fn content_fields_pipe_opened_has_pipe_id_and_omits_empty_label() {
        let content = Content::PipeOpened(PipeOpened {
            pipe_id: [0xef; 16],
            owner_id: IdentityKey::from_bytes([0x01; 32]),
            owner_endpoint: DeviceKey::from_bytes([0x02; 32]),
            kind: "tcp".to_string(),
            label: String::new(),
            target_hint: "127.0.0.1:3000".to_string(),
            alpn: "iroh-rooms/1".to_string(),
            allowed_members: vec![],
            expires_at: None,
        });
        let fields = content_fields(&content);
        let pipe_id = fields["pipe_id"]
            .as_str()
            .expect("pipe_id must be a string");
        assert_eq!(
            pipe_id.len(),
            32,
            "pipe_id must be 32 lowercase hex chars (16 bytes)"
        );
        assert!(
            pipe_id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "pipe_id must be lowercase hex: {pipe_id}"
        );
        assert!(!fields.contains_key("label"), "empty label must be omitted");
    }

    #[test]
    fn content_fields_pipe_opened_has_label_when_non_empty() {
        let content = Content::PipeOpened(PipeOpened {
            pipe_id: [0x12; 16],
            owner_id: IdentityKey::from_bytes([0x01; 32]),
            owner_endpoint: DeviceKey::from_bytes([0x02; 32]),
            kind: "tcp".to_string(),
            label: "dev-server".to_string(),
            target_hint: "127.0.0.1:8080".to_string(),
            alpn: "iroh-rooms/1".to_string(),
            allowed_members: vec![],
            expires_at: None,
        });
        let fields = content_fields(&content);
        assert_eq!(fields["label"].as_str(), Some("dev-server"));
    }

    // ── content_summary: untested variants ───────────────────────────────────

    #[test]
    fn content_summary_room_created_contains_name_label() {
        let content = Content::RoomCreated(RoomCreated {
            room_name: "Build Room".to_string(),
            room_nonce: [0u8; 16],
            admins: vec![],
            device_binding: dummy_binding(),
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("Build Room"),
            "summary for RoomCreated must contain the room name: {summary:?}"
        );
        assert!(
            summary.contains("name="),
            "summary for RoomCreated must include a name= label: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_invited_contains_invitee_and_role() {
        let content = Content::MemberInvited(MemberInvited {
            invite_id: [0u8; 16],
            capability_hash: [0u8; 32],
            role: "agent".to_string(),
            invitee_key: IdentityKey::from_bytes([0x09; 32]),
            expires_at: None,
            invitee_hint: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("role=agent"),
            "summary must include role=agent: {summary:?}"
        );
        assert!(
            summary.contains("invitee="),
            "summary must include invitee=: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_invited_with_expiry_contains_expires_label() {
        let content = Content::MemberInvited(MemberInvited {
            invite_id: [0u8; 16],
            capability_hash: [0u8; 32],
            role: "member".to_string(),
            invitee_key: IdentityKey::from_bytes([0x09; 32]),
            expires_at: Some(1_750_000_000_000),
            invitee_hint: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("expires="),
            "summary for MemberInvited with expiry must include expires=: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_joined_contains_role_and_display_name() {
        let content = Content::MemberJoined(MemberJoined {
            via_invite_id: [0u8; 16],
            capability_secret: [0u8; 16],
            role: "member".to_string(),
            device_binding: dummy_binding(),
            display_name: Some("Bob".to_string()),
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("role=member"),
            "summary must contain role=member: {summary:?}"
        );
        assert!(
            summary.contains("Bob"),
            "summary must contain the display_name: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_removed_contains_subject_and_by() {
        let content = Content::MemberRemoved(MemberRemoved {
            member_id: IdentityKey::from_bytes([0x07; 32]),
            removed_by: IdentityKey::from_bytes([0x08; 32]),
            reason: None,
            device_binding: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("subject="),
            "summary must contain subject=: {summary:?}"
        );
        assert!(
            summary.contains("by="),
            "summary must contain by=: {summary:?}"
        );
    }

    #[test]
    fn content_summary_member_removed_with_reason_contains_reason_text() {
        let content = Content::MemberRemoved(MemberRemoved {
            member_id: IdentityKey::from_bytes([0x07; 32]),
            removed_by: IdentityKey::from_bytes([0x08; 32]),
            reason: Some("abuse".to_string()),
            device_binding: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("abuse"),
            "summary must contain the reason text: {summary:?}"
        );
    }

    #[test]
    fn content_summary_file_shared_contains_name_and_size() {
        let content = Content::FileShared(FileShared {
            file_id: [0u8; 16],
            name: "notes.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size_bytes: 1024,
            blob_hash: HashRef::from_bytes([0xcc; 32]),
            blob_format: None,
            providers: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("notes.txt"),
            "summary must contain the file name: {summary:?}"
        );
        assert!(
            summary.contains("1024"),
            "summary must contain the size: {summary:?}"
        );
    }

    #[test]
    fn content_summary_pipe_opened_contains_pipe_label() {
        let content = Content::PipeOpened(PipeOpened {
            pipe_id: [0xef; 16],
            owner_id: IdentityKey::from_bytes([0x01; 32]),
            owner_endpoint: DeviceKey::from_bytes([0x02; 32]),
            kind: "tcp".to_string(),
            label: "dev-server".to_string(),
            target_hint: "127.0.0.1:3000".to_string(),
            alpn: "iroh-rooms/1".to_string(),
            allowed_members: vec![],
            expires_at: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("pipe="),
            "summary for PipeOpened must contain pipe=: {summary:?}"
        );
        assert!(
            summary.contains("dev-server"),
            "summary for PipeOpened must contain the label: {summary:?}"
        );
    }

    #[test]
    fn content_summary_pipe_closed_contains_pipe_prefix() {
        let content = Content::PipeClosed(PipeClosed {
            pipe_id: [0xab; 16],
            reason: None,
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("pipe="),
            "summary for PipeClosed must contain pipe=: {summary:?}"
        );
    }

    #[test]
    fn content_summary_pipe_closed_with_reason_contains_reason_and_label() {
        let content = Content::PipeClosed(PipeClosed {
            pipe_id: [0xab; 16],
            reason: Some("expired".to_string()),
        });
        let summary = content_summary(&content);
        assert!(
            summary.contains("expired"),
            "summary must contain the reason: {summary:?}"
        );
        assert!(
            summary.contains("reason="),
            "summary must contain reason= label: {summary:?}"
        );
    }
}
