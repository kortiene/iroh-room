//! CLI integration tests for `room join` (IR-0104).
//!
//! Coverage map (acceptance criteria and pre-IO gates):
//!   AC2 (wrong identity)        — `join_wrong_identity_exits_nonzero_with_actionable_message`
//!   bad-ticket pre-IO gate      — `join_garbage_ticket_exits_nonzero`
//!                                 `join_roomtkt1_garbled_body_exits_nonzero`
//!                                 `join_truncated_token_exits_nonzero`
//!   missing-identity pre-IO     — `join_without_identity_exits_nonzero`
//!   bad-timeout pre-IO gate     — `join_bad_timeout_exits_nonzero_before_io`
//!   secret hygiene (AC3)        — `join_wrong_identity_error_does_not_expose_secret_seeds`
//!   ticket token codec boundary — `join_wrong_identity_error_names_the_bound_key`
//!
//! Every test exits before any network or store IO: the ticket-decode and
//! key-binding pre-checks happen synchronously in `join::join` before the
//! ephemeral `Node` is brought up (spec D6 step 1).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

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

/// Extract the ticket value line (the trimmed line that follows `ticket:`).
fn extract_ticket(stdout: &str) -> Option<&str> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("ticket:") {
            return lines.next().map(str::trim);
        }
    }
    None
}

/// A fixed 64-hex-char key that is NOT produced by any deterministic seed used
/// in the test suite — serves as the invite's `--invitee` in admin-side setup.
const INVITEE_HEX: &str = "0404040404040404040404040404040404040404040404040404040404040404";

/// Stand up an admin home (identity + room + invite for `invitee_hex`) and
/// return the `roomtkt1…` ticket string.
fn admin_invite_ticket(home_admin: &TempDir, invitee_hex: &str) -> String {
    create_identity(home_admin);
    let room_id = create_room(home_admin);
    let out = cmd(home_admin)
        .args(["room", "invite", &room_id, "--invitee", invitee_hex])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "room invite must succeed to produce a ticket"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_ticket(&stdout)
        .expect("ticket must appear in `room invite` output")
        .to_owned()
}

// ── bad-ticket pre-IO gates ───────────────────────────────────────────────────

/// A completely random string must fail immediately with a decode error (the
/// ticket codec rejects it before the identity is loaded or any store is opened).
#[test]
fn join_garbage_ticket_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "notavalidticket"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ticket").or(predicate::str::contains("decode")));
}

/// A string with the `roomtkt1` HRP but a body containing characters outside
/// the RFC 4648 base32 alphabet (`!`, `@`, `1`) must fail with a decode error.
#[test]
fn join_roomtkt1_garbled_body_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "roomtkt1!!!invalid!!!"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ticket"));
}

/// `roomtkt1` prefix + a two-char base32 body — far too short to hold even the
/// version byte + 4-byte checksum. Must fail with a decode error.
#[test]
fn join_truncated_token_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "roomtkt1aa"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ticket"));
}

// ── missing-identity pre-IO gate ─────────────────────────────────────────────

/// A syntactically valid ticket tried from a home directory with no
/// `identity.secret` must fail before any network IO with a message that
/// references "identity". The ticket decodes successfully; the identity
/// load is the second pre-IO gate.
#[test]
fn join_without_identity_exits_nonzero() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    let home_joiner = TempDir::new().unwrap();
    cmd_at(home_joiner.path())
        .args(["room", "join", &ticket])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identity"));
}

// ── AC2: wrong-identity pre-IO gate ──────────────────────────────────────────

/// AC2: the ticket is key-bound to `INVITEE_HEX`, but the joiner's home holds a
/// freshly generated random identity (which is astronomically unlikely to equal
/// `[0x04; 32]`). The CLI must reject pre-IO with an actionable message before
/// any network connection is attempted.
#[test]
fn join_wrong_identity_exits_nonzero_with_actionable_message() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    let home_bob = TempDir::new().unwrap();
    cmd_at(home_bob.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    cmd_at(home_bob.path())
        .args(["room", "join", &ticket])
        .assert()
        .failure()
        // The message cites both identities so the user knows whose invite to request.
        .stderr(predicate::str::contains("identity"));
}

/// AC2: the wrong-identity error message must name the ticket's bound key so the
/// user can ask for the right invite.
#[test]
fn join_wrong_identity_error_names_the_bound_key() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    let home_bob = TempDir::new().unwrap();
    cmd_at(home_bob.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    let out = cmd_at(home_bob.path())
        .args(["room", "join", &ticket])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The error must cite the INVITEE_HEX key so the user knows whose invite was used.
    assert!(
        stderr.contains(INVITEE_HEX),
        "wrong-identity error must name the ticket's bound key so the user knows whose invite to request; got: {stderr}"
    );
}

/// AC3 (secret hygiene): the wrong-identity error message must not expose the
/// raw identity secret seeds stored in `identity.secret`.
#[test]
fn join_wrong_identity_error_does_not_expose_secret_seeds() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    let home_bob = TempDir::new().unwrap();
    cmd_at(home_bob.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    let secret_raw = std::fs::read_to_string(home_bob.path().join("identity.secret")).unwrap();
    let secret_v: serde_json::Value = serde_json::from_str(&secret_raw).unwrap();
    let identity_seed = secret_v["identity_secret"].as_str().unwrap().to_owned();
    let device_seed = secret_v["device_secret"].as_str().unwrap().to_owned();

    let out = cmd_at(home_bob.path())
        .args(["room", "join", &ticket])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stdout.contains(&identity_seed) && !stderr.contains(&identity_seed),
        "wrong-identity error must not expose the identity secret seed"
    );
    assert!(
        !stdout.contains(&device_seed) && !stderr.contains(&device_seed),
        "wrong-identity error must not expose the device secret seed"
    );
}

// ── bad-timeout pre-IO gate ───────────────────────────────────────────────────

/// The `--timeout` flag is parsed in `dispatch_room` before `join::join` is
/// called, so an invalid timeout string exits non-zero even for a garbage ticket.
/// No identity or network access required.
#[test]
fn join_bad_timeout_exits_nonzero_before_io() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "anytokenstring", "--timeout", "badvalue"])
        .assert()
        .failure();
}

/// Zero-duration timeout is not accepted.
#[test]
fn join_zero_duration_timeout_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "anytokenstring", "--timeout", "0s"])
        .assert()
        .failure();
}

// ── AC2: wrong-identity actionable message quality ────────────────────────────

/// AC2 (actionable message): the error must mention the admin and suggest
/// requesting a new invite, so the user knows the corrective action.
/// The code says: "Ask the admin to invite {`self_id`} instead."
#[test]
fn join_wrong_identity_error_suggests_corrective_action() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    let home_bob = TempDir::new().unwrap();
    cmd_at(home_bob.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    cmd_at(home_bob.path())
        .args(["room", "join", &ticket])
        .assert()
        .failure()
        // The error says "Ask the admin to invite {self_id} instead."
        .stderr(predicate::str::contains("admin").or(predicate::str::contains("invite")));
}

// ── Ticket codec edge-case via CLI ────────────────────────────────────────────

/// A ticket with leading/trailing whitespace must be rejected cleanly (the
/// code trims the ticket string before parsing, so trim-then-fail-closed).
#[test]
fn join_whitespace_padded_garbage_ticket_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "   notavalidticket   "])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ticket").or(predicate::str::contains("decode")));
}

/// The `roomtkt1` prefix alone (empty body) must fail with a decode error.
#[test]
fn join_bare_prefix_only_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "roomtkt1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("ticket"));
}
