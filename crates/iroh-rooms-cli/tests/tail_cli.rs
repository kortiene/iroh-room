//! CLI integration tests for `room tail --offline` (IR-0106 §9.1).
//!
//! Coverage map:
//!   basic success — exits 0, `type=room.created` present, blake3 event id
//!   attribution   — genesis row has `from=`, `role=admin`, `status=active`, `lamport=0`
//!   event order   — after `room send`, both events appear in ascending lamport order
//!   JSON mode     — `--offline --json` is a valid JSON array with correct fields
//!   determinism   — text and JSON output are byte-identical across invocations
//!   errors        — unknown room → non-zero + "no room"; malformed id → "invalid room id"
//!   clap flags    — `--offline --peer` rejected; `--json` without `--offline` rejected
//!   limit         — `--limit 0` shows nothing; `--limit 1` shows at most one event
//!   secret hygiene — offline tail loads no secrets, none appear in stdout/stderr
//!
//! Each test gets its own temp directory via `--data-dir` and clears `IROH_ROOMS_HOME`
//! so tests are fully isolated when run in parallel.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

use iroh_rooms_core::event::signed;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::{
    build_member_invited, build_member_joined, build_member_left, build_member_removed,
    build_room_created, capability_hash, DeviceBinding, SigningKey,
};
use iroh_rooms_core::store::EventStore;

// ── helpers ───────────────────────────────────────────────────────────────────

fn cmd(home: &TempDir) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(home.path());
    c
}

fn create_identity(home: &TempDir) {
    cmd(home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();
}

fn create_room(home: &TempDir) -> String {
    let out = cmd(home)
        .args(["room", "create", "Test Room"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "room create must succeed in test setup"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_field(&stdout, "room_id")
        .expect("room_id must appear in `room create` output")
        .to_owned()
}

fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

// A valid blake3:<hex> room id that does not exist in any test home.
// 64 lowercase hex chars (32 bytes) — correct RoomId format, unknown in any test store.
const FAKE_ROOM_ID: &str =
    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ── basic offline tail ────────────────────────────────────────────────────────

/// AC1: `room tail --offline` on a fresh room exits 0 (no error).
#[test]
fn tail_offline_fresh_room_exits_zero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success();
}

/// AC1 "validated events": the genesis event appears as `type=room.created`.
#[test]
fn tail_offline_shows_type_room_created_for_fresh_room() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type=room.created"));
}

/// The genesis row has `lamport=0` (the canonical first position).
#[test]
fn tail_offline_genesis_has_lamport_zero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lamport=0"));
}

/// The genesis row has a `event=blake3:` prefix (a valid signed event id).
#[test]
fn tail_offline_genesis_has_event_id_with_blake3_prefix() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("event=blake3:"));
}

/// AC1 attribution: the genesis row attributes to the admin (role=admin, status=active).
#[test]
fn tail_offline_genesis_has_admin_attribution() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=admin"))
        .stdout(predicate::str::contains("status=active"))
        .stdout(predicate::str::contains("from="));
}

// ── event ordering after send ─────────────────────────────────────────────────

/// After `room send`, the timeline contains both a `room.created` and a
/// `message.text` event.
#[test]
fn tail_offline_after_send_shows_both_event_types() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "send", &room_id, "first offline message"])
        .assert()
        .success();
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type=room.created"))
        .stdout(predicate::str::contains("type=message.text"));
}

/// AC1 deterministic order: genesis (lamport=0) appears before the message
/// (lamport=1) in the text output.
#[test]
fn tail_offline_events_appear_in_ascending_lamport_order() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "send", &room_id, "order probe"])
        .assert()
        .success();
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let genesis_pos = stdout
        .find("lamport=0")
        .expect("lamport=0 must appear for the genesis event");
    let msg_pos = stdout
        .find("lamport=1")
        .expect("lamport=1 must appear for the message event");
    assert!(
        genesis_pos < msg_pos,
        "genesis (lamport=0) must appear before the message (lamport=1)"
    );
}

// ── JSON output ───────────────────────────────────────────────────────────────

/// AC4: `room tail --offline --json` exits 0.
#[test]
fn tail_offline_json_mode_exits_zero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .assert()
        .success();
}

/// AC4: the `--json` output is a valid JSON array.
#[test]
fn tail_offline_json_is_a_valid_json_array() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json output must be valid JSON");
    assert!(
        parsed.is_array(),
        "--json output must be a JSON array, got: {parsed}"
    );
}

/// The first element of the JSON array for a fresh room has `event_type: "room.created"`.
#[test]
fn tail_offline_json_genesis_has_event_type_room_created() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        !rows.is_empty(),
        "JSON array must be non-empty for a fresh room"
    );
    assert_eq!(
        rows[0]["event_type"].as_str(),
        Some("room.created"),
        "first row must have event_type=room.created"
    );
}

/// The genesis JSON row has `lamport: 0`.
#[test]
fn tail_offline_json_genesis_has_lamport_zero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        rows[0]["lamport"].as_u64(),
        Some(0),
        "genesis lamport must be 0"
    );
}

/// The genesis JSON row has correct attribution fields.
#[test]
fn tail_offline_json_genesis_has_admin_attribution_fields() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        rows[0]["role"].as_str(),
        Some("admin"),
        "genesis row role must be admin"
    );
    assert_eq!(
        rows[0]["status"].as_str(),
        Some("active"),
        "genesis row status must be active"
    );
    assert!(
        rows[0]["from"].is_string(),
        "genesis row must have a 'from' string field"
    );
    assert!(
        rows[0]["event_id"]
            .as_str()
            .is_some_and(|s| s.starts_with("blake3:")),
        "event_id must start with blake3:"
    );
}

/// After `room send`, the JSON array contains a `message.text` row with the correct `body`.
#[test]
fn tail_offline_json_message_row_has_body_field() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "send", &room_id, "structured test content"])
        .assert()
        .success();
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    let msg_row = rows
        .iter()
        .find(|r| r["event_type"].as_str() == Some("message.text"))
        .expect("message.text row must be present after send");
    assert_eq!(
        msg_row["body"].as_str(),
        Some("structured test content"),
        "message.text row must carry the sent body"
    );
    assert!(
        msg_row["format"].is_string(),
        "format field must be present in message.text row"
    );
}

// ── restart determinism ───────────────────────────────────────────────────────

/// AC1: two `room tail --offline` runs over the same `rooms.db` yield byte-identical output.
#[test]
fn tail_offline_text_output_is_deterministic_across_restarts() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out1 = cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let out2 = cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    assert_eq!(
        out1.stdout, out2.stdout,
        "offline text output must be byte-identical across invocations"
    );
}

/// AC4 + determinism: `--offline --json` output is stable across invocations.
#[test]
fn tail_offline_json_output_is_deterministic_across_restarts() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out1 = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let out2 = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    assert_eq!(
        out1.stdout, out2.stdout,
        "offline --json output must be byte-identical across invocations"
    );
}

// ── errors ────────────────────────────────────────────────────────────────────

/// An unknown (but well-formed) room id exits non-zero with "no room" in stderr.
#[test]
fn tail_offline_unknown_room_exits_nonzero_with_actionable_message() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    create_room(&home); // so rooms.db exists
    cmd(&home)
        .args(["room", "tail", FAKE_ROOM_ID, "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));
}

/// A syntactically malformed room id exits non-zero with "invalid room id" in stderr.
#[test]
fn tail_offline_malformed_room_id_exits_nonzero_with_hint() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "tail", "not-a-valid-id", "--offline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid room id"));
}

/// Without any room created, `--offline` fails (no room in store).
#[test]
fn tail_offline_without_prior_room_create_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "tail", FAKE_ROOM_ID, "--offline"])
        .assert()
        .failure();
}

// ── clap flag conflicts ───────────────────────────────────────────────────────

/// `--offline` conflicts with `--peer`; clap must reject the combination.
#[test]
fn tail_offline_conflicts_with_peer_flag() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "room",
            "tail",
            FAKE_ROOM_ID,
            "--offline",
            "--peer",
            "127.0.0.1:1234",
        ])
        .assert()
        .failure();
}

/// `--json` requires `--offline`; using `--json` alone must be rejected by clap.
#[test]
fn tail_json_requires_offline_flag() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "tail", FAKE_ROOM_ID, "--json"])
        .assert()
        .failure();
}

/// `--offline` conflicts with `--accept-joins`; clap must reject the combination.
#[test]
fn tail_offline_conflicts_with_accept_joins_flag() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "tail", FAKE_ROOM_ID, "--offline", "--accept-joins"])
        .assert()
        .failure();
}

// ── limit flag ────────────────────────────────────────────────────────────────

/// `--limit 0` produces no text lines (the store returns 0 rows).
#[test]
fn tail_offline_limit_zero_shows_no_events() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--limit", "0"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "--limit 0 must produce no output in text mode, got: {stdout:?}"
    );
}

/// `--limit 1` produces at most 1 text line even when 3 events are in the log.
#[test]
fn tail_offline_limit_one_shows_at_most_one_event() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    // Add two messages so we have 3 events total (genesis + 2 messages).
    cmd(&home)
        .args(["room", "send", &room_id, "first"])
        .assert()
        .success();
    cmd(&home)
        .args(["room", "send", &room_id, "second"])
        .assert()
        .success();
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--limit", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "--limit 1 must produce exactly one output line, got {} lines:\n{stdout}",
        lines.len()
    );
}

// ── secret hygiene ────────────────────────────────────────────────────────────

/// Offline tail loads no secret key material; identity and device seeds must
/// never appear in stdout or stderr (spec D7).
#[test]
fn tail_offline_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret"))
        .expect("identity.secret must exist");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("identity.secret must be valid JSON");
    let identity_seed = secret_v["identity_secret"]
        .as_str()
        .expect("identity_secret field")
        .to_owned();
    let device_seed = secret_v["device_secret"]
        .as_str()
        .expect("device_secret field")
        .to_owned();

    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stdout.contains(&identity_seed),
        "offline tail stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(&identity_seed),
        "offline tail stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(&device_seed),
        "offline tail stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(&device_seed),
        "offline tail stderr must not contain the device secret seed"
    );
}

// ── JSON field content ────────────────────────────────────────────────────────

/// The genesis JSON row carries a `room_name` field equal to the name passed at
/// create (`content_fields` projection for room.created events).
#[test]
fn tail_offline_json_genesis_has_room_name_field() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let out = cmd(&home)
        .args(["room", "create", "Engineering HQ"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let room_id = extract_field(&stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();

    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        !rows.is_empty(),
        "JSON array must be non-empty for a fresh room"
    );
    assert_eq!(
        rows[0]["room_name"].as_str(),
        Some("Engineering HQ"),
        "genesis row room_name must equal the room's creation name"
    );
}

/// The genesis JSON row has an `at` field that looks like an ISO-8601 UTC timestamp.
#[test]
fn tail_offline_json_genesis_has_iso8601_at_field() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    let at = rows[0]["at"].as_str().expect("'at' field must be a string");
    assert!(
        at.ends_with('Z') && at.contains('T') && at.len() >= 20,
        "'at' must look like an ISO-8601 UTC timestamp (e.g. 2025-01-01T00:00:00Z), got: {at:?}"
    );
}

/// The genesis JSON row has a `created_at` field that is a positive ms-epoch integer.
#[test]
fn tail_offline_json_genesis_has_positive_created_at() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    let created_at = rows[0]["created_at"]
        .as_u64()
        .expect("'created_at' must be a u64");
    assert!(
        created_at > 0,
        "'created_at' must be a positive ms-since-epoch timestamp, got: {created_at}"
    );
}

/// The genesis JSON row's `from` field is exactly 8 lowercase hex chars (the short
/// sender id format documented in spec D6).
#[test]
fn tail_offline_json_genesis_from_is_8_char_lowercase_hex() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    let from = rows[0]["from"].as_str().expect("'from' must be a string");
    assert_eq!(
        from.len(),
        8,
        "'from' must be exactly 8 chars (first 8 hex of sender_id), got: {from:?}"
    );
    assert!(
        from.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "'from' must be lowercase hex, got: {from:?}"
    );
}

/// `--offline --json --limit 1` returns a JSON array with exactly 1 element even
/// when 2 events (genesis + message) are stored.
#[test]
fn tail_offline_json_limit_one_returns_single_element_array() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "send", &room_id, "any message"])
        .assert()
        .success();
    let out = cmd(&home)
        .args([
            "room",
            "tail",
            &room_id,
            "--offline",
            "--json",
            "--limit",
            "1",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        rows.len(),
        1,
        "--limit 1 with --json must produce a 1-element array, got {} elements",
        rows.len()
    );
}

/// Text output for a `message.text` event includes `body=<message>` in the
/// content summary (verifies the `content_summary` projection is wired to the tail).
#[test]
fn tail_offline_text_shows_message_body_in_content_summary() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    cmd(&home)
        .args(["room", "send", &room_id, "hello from tests"])
        .assert()
        .success();
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("body=hello from tests"));
}

/// Text output for the genesis event includes the room name in the content summary
/// (verifies the room.created `content_summary` projection is correct end-to-end).
#[test]
fn tail_offline_text_genesis_shows_room_name_in_content_summary() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let out = cmd(&home)
        .args(["room", "create", "Named Room"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let room_id = extract_field(&stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Named Room"));
}

// ── AC3: removed / left member representation ─────────────────────────────────
//
// These tests seed a `rooms.db` directly using the core event builders (no CLI
// invite/join needed) because there is no `room leave` / `member remove` command
// yet. The chain is: genesis → invite → join → departure (left | removed |
// concurrent left + removed). The CLI commands `room tail --offline` and
// `room tail --offline --json` are then called over the seeded store and their
// output is asserted.

/// Fixed seeds for a deterministic, repeatable test chain.
const AC3_ADMIN_SEED: [u8; 32] = [0x10; 32];
const AC3_ADMIN_DEV_SEED: [u8; 32] = [0x11; 32];
const AC3_MEMBER_SEED: [u8; 32] = [0x14; 32];
const AC3_MEMBER_DEV_SEED: [u8; 32] = [0x15; 32];
const AC3_ROOM_NONCE: [u8; 16] = [0xbb; 16];
const AC3_INVITE_ID: [u8; 16] = [0xda; 16];
const AC3_CAP_SECRET: [u8; 16] = [0x5e; 16];
const AC3_BASE_TS: u64 = 1_750_000_000_000;

/// Which kind of departure event to append after the join.
#[derive(Copy, Clone)]
enum Departure {
    /// Bob voluntarily self-departs (`member.left`).
    Left,
    /// Alice (admin) removes Bob (`member.removed`).
    Removed,
    /// Both a self-leave and a concurrent admin-removal target Bob — admin-removal
    /// must dominate the display (D5).
    ConcurrentLeftAndRemoved,
}

/// Seed a `rooms.db` in `home` with the chain
/// `genesis → invite → join → departure(kind)` and return the room id string.
///
/// Uses fixed-seed keys so the chain is deterministic across runs. The seeded
/// directory has no `identity.secret` — the offline read commands do not require
/// one (spec D7).
#[allow(clippy::too_many_lines)]
fn seed_departed_member(home: &TempDir, departure: Departure) -> String {
    let admin_id = SigningKey::from_seed(&AC3_ADMIN_SEED);
    let admin_dev = SigningKey::from_seed(&AC3_ADMIN_DEV_SEED);
    let member_id = SigningKey::from_seed(&AC3_MEMBER_SEED);
    let member_dev = SigningKey::from_seed(&AC3_MEMBER_DEV_SEED);

    let room_id = signed::derive_room_id(&admin_id.identity_key(), &AC3_ROOM_NONCE, AC3_BASE_TS);
    let ctx = ValidationContext::for_room(room_id);
    let db_path = home.path().join("rooms.db");
    let mut store = EventStore::open(&db_path).expect("open store");

    // Genesis
    let genesis_wire = build_room_created(
        &admin_id,
        &admin_dev,
        "AC3 Room",
        &AC3_ROOM_NONCE,
        AC3_BASE_TS,
    );
    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("validate genesis");
    let genesis_ev_id = genesis_v.event_id;
    store.insert(&genesis_v).expect("insert genesis");

    // Invite (Bob)
    let cap_hash = capability_hash(&room_id, &AC3_INVITE_ID, &AC3_CAP_SECRET);
    let invite_wire = build_member_invited(
        &admin_id,
        &admin_dev,
        &room_id,
        &AC3_INVITE_ID,
        &cap_hash,
        "member",
        &member_id.identity_key(),
        None,
        None,
        &[genesis_ev_id],
        AC3_BASE_TS + 1_000,
    );
    let invite_v = validate_wire_bytes(&invite_wire.to_bytes(), &ctx).expect("validate invite");
    let invite_ev_id = invite_v.event_id;
    store.insert(&invite_v).expect("insert invite");

    // Join (Bob)
    let binding = DeviceBinding::create(&room_id, &member_id, member_dev.device_key());
    let join_wire = build_member_joined(
        &member_id,
        &member_dev,
        &room_id,
        &AC3_INVITE_ID,
        &AC3_CAP_SECRET,
        "member",
        binding,
        Some("Bob"),
        &[invite_ev_id],
        AC3_BASE_TS + 2_000,
    );
    let join_v = validate_wire_bytes(&join_wire.to_bytes(), &ctx).expect("validate join");
    let join_ev_id = join_v.event_id;
    store.insert(&join_v).expect("insert join");

    // Departure
    match departure {
        Departure::Left => {
            let left_wire = build_member_left(
                &member_id,
                &member_dev,
                &room_id,
                None,
                &[join_ev_id],
                AC3_BASE_TS + 3_000,
            );
            let left_v = validate_wire_bytes(&left_wire.to_bytes(), &ctx).expect("validate left");
            store.insert(&left_v).expect("insert left");
        }
        Departure::Removed => {
            let removed_wire = build_member_removed(
                &admin_id,
                &admin_dev,
                &room_id,
                &member_id.identity_key(),
                None,
                None,
                &[join_ev_id],
                AC3_BASE_TS + 3_000,
            );
            let removed_v =
                validate_wire_bytes(&removed_wire.to_bytes(), &ctx).expect("validate removed");
            store.insert(&removed_v).expect("insert removed");
        }
        Departure::ConcurrentLeftAndRemoved => {
            // Both events cite the join as parent (concurrent departures).
            // Admin-removal must dominate in the display (D5 dominance rule).
            let left_wire = build_member_left(
                &member_id,
                &member_dev,
                &room_id,
                None,
                &[join_ev_id],
                AC3_BASE_TS + 3_000,
            );
            let left_v = validate_wire_bytes(&left_wire.to_bytes(), &ctx).expect("validate left");
            store.insert(&left_v).expect("insert left");

            let removed_wire = build_member_removed(
                &admin_id,
                &admin_dev,
                &room_id,
                &member_id.identity_key(),
                None,
                None,
                &[join_ev_id],
                AC3_BASE_TS + 3_000,
            );
            let removed_v =
                validate_wire_bytes(&removed_wire.to_bytes(), &ctx).expect("validate removed");
            store.insert(&removed_v).expect("insert removed");
        }
    }

    room_id.to_string()
}

/// AC3: a voluntarily departed member's events show `status=left` in offline tail.
#[test]
fn tail_offline_voluntary_departure_shows_status_left() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=left"));
}

/// AC3: an admin-removed member's events show `status=removed` in offline tail.
#[test]
fn tail_offline_admin_removal_shows_status_removed() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Removed);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=removed"));
}

/// AC3: the departure event itself (member.left) is not silently omitted.
#[test]
fn tail_offline_departure_event_is_not_omitted() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type=member.left"));
}

/// AC3: the removal event itself (member.removed) appears in the offline tail.
#[test]
fn tail_offline_removal_event_is_not_omitted() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Removed);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type=member.removed"));
}

/// AC3 + AC4: JSON mode shows `"status":"left"` for a voluntarily departed member.
#[test]
fn tail_offline_json_voluntary_departure_has_left_status() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "room tail --offline --json must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("--json output must be a valid JSON array");
    // At least one row should be attributed to the departed member with status=left.
    let has_left = rows.iter().any(|r| r["status"].as_str() == Some("left"));
    assert!(
        has_left,
        "at least one JSON row must have status=left for a voluntarily departed member; rows: {rows:#?}"
    );
}

/// AC3 + AC4: JSON mode shows `"status":"removed"` for an admin-removed member.
#[test]
fn tail_offline_json_admin_removal_has_removed_status() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Removed);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("--json output must be a valid JSON array");
    let has_removed = rows.iter().any(|r| r["status"].as_str() == Some("removed"));
    assert!(
        has_removed,
        "at least one JSON row must have status=removed for an admin-removed member; rows: {rows:#?}"
    );
}

/// AC3 D5 dominance: when both a self-leave and a concurrent admin-removal target
/// the same member, the offline tail must show `status=removed`, not `status=left`.
#[test]
fn tail_offline_admin_removal_dominates_concurrent_self_leave() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::ConcurrentLeftAndRemoved);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("--json output must be a valid JSON array");
    // All rows attributed to the departed member must show status=removed, not left.
    // (The member's join row and message rows if any must reflect admin-removal.)
    let member_id_hex = SigningKey::from_seed(&AC3_MEMBER_SEED)
        .identity_key()
        .to_string();
    let member_short = &member_id_hex[..8];
    let member_rows: Vec<_> = rows
        .iter()
        .filter(|r| r["from"].as_str() == Some(member_short))
        .collect();
    assert!(
        !member_rows.is_empty(),
        "the joined member must appear in at least one row"
    );
    for row in &member_rows {
        assert_eq!(
            row["status"].as_str(),
            Some("removed"),
            "when admin-removal and self-leave are concurrent, all member rows must show \
             status=removed (D5 dominance); row: {row:#?}"
        );
    }
}

// ── AC1 "validated events": member.invited and member.joined ──────────────────
//
// These tests reuse the full membership chain seeded by `seed_departed_member`
// (genesis → invite → join → departure) to verify that `member.invited` and
// `member.joined` event types appear in both text and JSON tail output, and that
// their lamport positions are strictly ascending together with the other types.

/// AC1: a `member.invited` event appears with `type=member.invited` in text output.
#[test]
fn tail_offline_shows_type_member_invited() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type=member.invited"));
}

/// AC1: a `member.joined` event appears with `type=member.joined` in text output.
#[test]
fn tail_offline_shows_type_member_joined() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type=member.joined"));
}

/// AC1 ordering: for a full genesis → invite → join → left chain, all four event types appear
/// and their lamport positions are strictly ascending (0 < 1 < 2 < 3).
#[test]
fn tail_offline_complete_chain_all_event_types_appear_in_ascending_lamport_order() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("type=room.created"),
        "room.created must appear in the full chain tail"
    );
    assert!(
        stdout.contains("type=member.invited"),
        "member.invited must appear in the full chain tail"
    );
    assert!(
        stdout.contains("type=member.joined"),
        "member.joined must appear in the full chain tail"
    );
    assert!(
        stdout.contains("type=member.left"),
        "member.left must appear in the full chain tail"
    );
    let pos0 = stdout.find("lamport=0").expect("lamport=0 for genesis");
    let pos1 = stdout.find("lamport=1").expect("lamport=1 for invite");
    let pos2 = stdout.find("lamport=2").expect("lamport=2 for join");
    let pos3 = stdout.find("lamport=3").expect("lamport=3 for departure");
    assert!(
        pos0 < pos1,
        "genesis (lamport=0) must precede invite (lamport=1)"
    );
    assert!(
        pos1 < pos2,
        "invite (lamport=1) must precede join (lamport=2)"
    );
    assert!(
        pos2 < pos3,
        "join (lamport=2) must precede departure (lamport=3)"
    );
}

/// AC4: `room tail --offline --json` for the full chain includes a `member.invited` row.
#[test]
fn tail_offline_json_includes_member_invited_row() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("valid JSON array");
    assert!(
        rows.iter()
            .any(|r| r["event_type"].as_str() == Some("member.invited")),
        "JSON tail must include a member.invited row; rows: {rows:#?}"
    );
}

/// AC4: `room tail --offline --json` for the full chain includes a `member.joined` row.
#[test]
fn tail_offline_json_includes_member_joined_row() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("valid JSON array");
    assert!(
        rows.iter()
            .any(|r| r["event_type"].as_str() == Some("member.joined")),
        "JSON tail must include a member.joined row; rows: {rows:#?}"
    );
}

/// AC4 + attribution: the `member.invited` JSON row carries `invitee` and `invited_role`
/// content fields from the `content_fields` projection.
#[test]
fn tail_offline_json_member_invited_row_has_invitee_and_role_fields() {
    let home = TempDir::new().unwrap();
    let room_id = seed_departed_member(&home, Departure::Left);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("valid JSON array");
    let invite_row = rows
        .iter()
        .find(|r| r["event_type"].as_str() == Some("member.invited"))
        .expect("member.invited row must be present");
    assert!(
        invite_row["invitee"].is_string(),
        "member.invited row must have an 'invitee' string field; row: {invite_row:#?}"
    );
    assert_eq!(
        invite_row["invited_role"].as_str(),
        Some("member"),
        "member.invited row must have invited_role='member'; row: {invite_row:#?}"
    );
}

/// AC4 + attribution: when a member joins with a display name, at least one of their tail rows
/// carries the `display_name` field set to that name.
#[test]
fn tail_offline_json_member_rows_carry_display_name_when_joined_with_name() {
    let home = TempDir::new().unwrap();
    // seed_departed_member calls build_member_joined with display_name = Some("Bob").
    let room_id = seed_departed_member(&home, Departure::Left);
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("valid JSON array");
    let member_short = &SigningKey::from_seed(&AC3_MEMBER_SEED)
        .identity_key()
        .to_string()[..8];
    let member_rows: Vec<_> = rows
        .iter()
        .filter(|r| r["from"].as_str() == Some(member_short))
        .collect();
    assert!(
        !member_rows.is_empty(),
        "the joined member must appear in at least one row; all rows: {rows:#?}"
    );
    let has_display_name = member_rows
        .iter()
        .any(|r| r["display_name"].as_str() == Some("Bob"));
    assert!(
        has_display_name,
        "at least one row attributed to Bob must carry display_name='Bob'; \
         Bob's rows: {member_rows:#?}"
    );
}
