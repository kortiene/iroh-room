//! CLI integration tests for `room send` (IR-0105 §9.2).
//!
//! Coverage map (spec §9.2 / acceptance criteria):
//!   Pre-IO gates  — empty body, over-cap body, bad --format, bad --reply-to,
//!                   bad --timeout: each exits non-zero before any IO
//!   No identity   — exits non-zero with actionable message
//!   Unknown room  — exits non-zero; nothing written to any home
//!   Happy path    — offline-only send (no peers): stored locally, labeled output
//!   Secret hygiene (§8) — device and identity seeds absent from stdout/stderr

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

fn cmd(home: &TempDir) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(home.path());
    c
}

fn cmd_at(path: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME").arg("--data-dir").arg(path);
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
    assert!(out.status.success(), "room create must succeed");
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

/// A syntactically valid `blake3:<hex>` room id that does not exist in any test home.
const FAKE_ROOM_ID: &str =
    "blake3:abababababababababababababababababababababababababababababababab";

// ── pre-IO gate: empty body → exits 1, writes nothing ────────────────────────

#[test]
fn send_empty_body_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "send", FAKE_ROOM_ID, ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("empty"));
}

// ── pre-IO gate: over-cap body → exits 1, mentions the cap ───────────────────

#[test]
fn send_over_cap_body_exits_nonzero() {
    let home = TempDir::new().unwrap();
    let big_body = "a".repeat(16_385); // MAX_MESSAGE_BODY_BYTES + 1
    cmd(&home)
        .args(["room", "send", FAKE_ROOM_ID, &big_body])
        .assert()
        .failure()
        .stderr(predicate::str::contains("16384"));
}

// ── pre-IO gate: bad --format → exits 1, names the rejected value ────────────

#[test]
fn send_bad_format_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "send", FAKE_ROOM_ID, "hello", "--format", "html"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("html"));
}

// ── pre-IO gate: bad --reply-to → exits 1, hints at blake3 prefix ────────────

#[test]
fn send_bad_reply_to_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "room",
            "send",
            FAKE_ROOM_ID,
            "hello",
            "--reply-to",
            "not-an-event-id",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("blake3"));
}

// ── pre-IO gate: bad --timeout → exits 1 ──────────────────────────────────────

#[test]
fn send_bad_timeout_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "send", FAKE_ROOM_ID, "hello", "--timeout", "soon"])
        .assert()
        .failure();
}

// ── no identity → exits 1 ─────────────────────────────────────────────────────

#[test]
fn send_without_identity_exits_nonzero() {
    let home = TempDir::new().unwrap();
    // Empty home: no identity created, no room. Must fail before any store IO.
    cmd(&home)
        .args(["room", "send", FAKE_ROOM_ID, "hello"])
        .assert()
        .failure();
}

// ── unknown room (identity exists, room not) → exits 1 ───────────────────────

#[test]
fn send_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "send", FAKE_ROOM_ID, "hello"])
        .assert()
        .failure();
}

// ── happy path: offline-only send (no peers) ──────────────────────────────────
//
// Sends a message with no --peer flags. The creator is the only Active member,
// so dial_set is empty → local-only persist branch (no network operations).
// The command must exit 0 and print the mandatory labeled output fields.

#[test]
fn send_offline_stores_locally_and_prints_required_fields() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "send", &room_id, "hello offline"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "offline send must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        extract_field(&stdout, "sent").is_some(),
        "output must contain 'sent: <event_id>'"
    );
    assert_eq!(
        extract_field(&stdout, "room"),
        Some(room_id.as_str()),
        "output 'room:' must match the room id"
    );
    assert_eq!(
        extract_field(&stdout, "stored"),
        Some("yes"),
        "output must contain 'stored: yes'"
    );
    assert!(
        stdout.contains("delivered: 0"),
        "no peers → output must contain 'delivered: 0'"
    );
}

// ── happy path: sent event id has blake3 prefix ───────────────────────────────

#[test]
fn send_reports_event_id_with_blake3_prefix() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "send", &room_id, "event id prefix test"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let event_id = extract_field(&stdout, "sent").expect("'sent:' field must be present");
    assert!(
        event_id.starts_with("blake3:"),
        "event_id must start with 'blake3:'; got {event_id:?}"
    );
}

// ── isolation: a room in home1 is not visible from home2 ──────────────────────

#[test]
fn send_data_dir_isolates_homes() {
    let home1 = TempDir::new().unwrap();
    let home2 = TempDir::new().unwrap();
    create_identity(&home1);
    let room_id = create_room(&home1);

    // Create a separate identity in home2; the room only exists in home1.
    cmd_at(home2.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();
    cmd_at(home2.path())
        .args(["room", "send", &room_id, "cross-home"])
        .assert()
        .failure();
}

// ── secret hygiene: identity and device seeds absent from output ──────────────

#[test]
fn send_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let secret_raw =
        std::fs::read_to_string(home.path().join("identity.secret")).expect("identity.secret");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("parse identity.secret");
    let identity_seed = secret_v["identity_secret"]
        .as_str()
        .expect("identity_secret field")
        .to_owned();
    let device_seed = secret_v["device_secret"]
        .as_str()
        .expect("device_secret field")
        .to_owned();

    let out = cmd(&home)
        .args(["room", "send", &room_id, "seed hygiene test"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(&identity_seed),
        "stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(&identity_seed),
        "stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(&device_seed),
        "stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(&device_seed),
        "stderr must not contain the device secret seed"
    );
}

// ── two consecutive offline sends both succeed ────────────────────────────────
//
// Regression guard: the second send must not fail because the first one left
// the store in a bad state (e.g. the store was not properly closed).

#[test]
fn send_two_consecutive_messages_both_succeed() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out1 = cmd(&home)
        .args(["room", "send", &room_id, "first message"])
        .output()
        .unwrap();
    assert!(
        out1.status.success(),
        "first send must exit 0; stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    let out2 = cmd(&home)
        .args(["room", "send", &room_id, "second message"])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "second send must exit 0; stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Each send must produce a distinct event id.
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let id1 = extract_field(&stdout1, "sent").expect("first 'sent:' field");
    let id2 = extract_field(&stdout2, "sent").expect("second 'sent:' field");
    assert_ne!(
        id1, id2,
        "two distinct messages must produce distinct event ids"
    );
}

// ── --format markdown is accepted and reported as stored ──────────────────────

#[test]
fn send_with_format_markdown_succeeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args([
            "room",
            "send",
            &room_id,
            "**bold** message",
            "--format",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "send with --format markdown must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        extract_field(&stdout, "stored"),
        Some("yes"),
        "markdown message must be stored locally"
    );
}

// ── --reply-to with a valid event id is accepted ──────────────────────────────

#[test]
fn send_with_valid_reply_to_succeeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    // First, send a message to get a real event id to reply to.
    let first_out = cmd(&home)
        .args(["room", "send", &room_id, "original"])
        .output()
        .unwrap();
    assert!(first_out.status.success());
    let first_stdout = String::from_utf8_lossy(&first_out.stdout);
    let reply_to_id = extract_field(&first_stdout, "sent").expect("'sent:' field");

    // Reply to the first message.
    let out = cmd(&home)
        .args([
            "room",
            "send",
            &room_id,
            "reply message",
            "--reply-to",
            reply_to_id,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "send with --reply-to a valid id must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(extract_field(&stdout, "stored"), Some("yes"));
}
