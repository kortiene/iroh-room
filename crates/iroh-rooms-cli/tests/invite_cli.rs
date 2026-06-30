//! CLI integration tests for `room invite` (IR-0103 §11).
//!
//! Each test gets its own temp directory via `--data-dir` so tests run fully
//! isolated even in parallel. `IROH_ROOMS_HOME` is cleared everywhere.
//!
//! Coverage map (spec §11 / acceptance criteria):
//!   AC1 (admin-only)   — `invite_non_admin_exits_nonzero_with_actionable_message`
//!                        + `invite_unknown_room_exits_nonzero`
//!   AC2 (key-bound)    — `invite_output_contains_invitee_key`
//!                        + `invite_invitee_appears_in_members_as_invited`
//!   AC3 (secret hygiene) — `invite_does_not_expose_secret_seeds`
//!   AC4 (hash)         — tested at the core level (event/invite.rs, ticket.rs)
//!   AC5 (expiry)       — `invite_with_expiry_shows_absolute_and_relative_duration`
//!                        + `invite_without_expiry_shows_never`
//!   happy path         — fields, ticket prefix, role=agent, db growth, restart
//!   pre-IO gates (D7–D9) — bad expires, bad invitee, self-invite, --role admin
//!   isolation          — `data_dir_flag_isolates_invites`

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

/// Create an identity named "Alice" in `home`. Panics if the command fails.
fn create_identity(home: &TempDir) {
    cmd(home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();
}

/// Run `room create` in `home` and return the printed `room_id`. Panics on failure.
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

/// Run `identity show` in `home` and return the `identity_id`. Panics on failure.
fn get_identity_id(home: &TempDir) -> String {
    let out = cmd(home).args(["identity", "show"]).output().unwrap();
    assert!(out.status.success(), "identity show must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_field(&stdout, "identity_id")
        .expect("identity_id must appear in `identity show` output")
        .to_owned()
}

/// Extract the value of the first `key: value` line in `output`.
fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

/// Extract the ticket token from the output: the trimmed line that follows
/// the `ticket:` label line.
fn extract_ticket(stdout: &str) -> Option<&str> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("ticket:") {
            return lines.next().map(str::trim);
        }
    }
    None
}

/// A fixed 64-hex-char invitee key (32 raw bytes). `IdentityKey::from_bytes` does
/// not validate curve-point membership, so any well-formed 32-byte hex works for
/// invite tests — this value differs from any CSPRNG-generated admin key.
const INVITEE_HEX: &str = "0404040404040404040404040404040404040404040404040404040404040404";

// ── happy path ────────────────────────────────────────────────────────────────

/// Successful `room invite` exits 0 and prints all mandatory output fields.
#[test]
fn invite_exits_zero_and_prints_required_fields() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .success()
        .stdout(predicate::str::contains("invite_id:"))
        .stdout(predicate::str::contains("room:"))
        .stdout(predicate::str::contains("invitee:"))
        .stdout(predicate::str::contains("role:"))
        .stdout(predicate::str::contains("expires:"))
        .stdout(predicate::str::contains("ticket:"))
        .stdout(predicate::str::contains("warning:"));
}

/// AC2: the `invitee:` field in the output must echo the `--invitee` key
/// exactly, proving the key was bound into the invite.
#[test]
fn invite_output_contains_invitee_key() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let invitee = extract_field(&stdout, "invitee").expect("invitee: field must appear in output");
    assert_eq!(
        invitee, INVITEE_HEX,
        "invitee: line must echo the --invitee key exactly (AC2)"
    );
}

/// The ticket token printed after `ticket:` must start with the canonical
/// `roomtkt1` HRP (spec D4 / ticket.rs).
#[test]
fn invite_output_ticket_has_roomtkt1_prefix() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let token = extract_ticket(&stdout).expect("a ticket value line must follow 'ticket:'");
    assert!(
        token.starts_with("roomtkt1"),
        "ticket token must start with 'roomtkt1' but got: {token}"
    );
}

/// AC5: `--expires 24h` must print an ISO-8601 UTC timestamp and the `(in 24h)`
/// human-readable annotation on the `expires:` line.
#[test]
fn invite_with_expiry_shows_absolute_and_relative_duration() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            INVITEE_HEX,
            "--expires",
            "24h",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let expires_line = stdout
        .lines()
        .find(|l| l.starts_with("expires:"))
        .expect("expires: line must appear in output");

    assert!(
        expires_line.contains("(in 24h)"),
        "expires: line must contain the '(in 24h)' annotation (AC5): {expires_line}"
    );
    // ISO-8601 UTC format: contains 'T' and ends with 'Z'.
    assert!(
        expires_line.contains('T') && expires_line.contains('Z'),
        "expires: line must contain an ISO-8601 timestamp (AC5): {expires_line}"
    );
}

/// No `--expires` → `expires: never` (no absolute timestamp printed).
#[test]
fn invite_without_expiry_shows_never() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .success()
        .stdout(predicate::str::contains("expires: never"));
}

/// `--role agent` is accepted (the second permitted role beside the default `member`).
#[test]
fn invite_role_agent_is_accepted() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            INVITEE_HEX,
            "--role",
            "agent",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("role: agent"));
}

/// A successful invite must persist a new event to `rooms.db`. The database
/// file must exist after the command, and the membership view (re-derived from
/// the persisted log) must reflect the invited identity.
#[test]
fn invite_persists_invite_event_to_rooms_db() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .success();

    // rooms.db must exist.
    assert!(
        home.path().join("rooms.db").exists(),
        "rooms.db must exist after a successful invite"
    );
    // The fold re-derives from the persisted store: invitee must appear.
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains(INVITEE_HEX));
}

/// AC2 + fold integration: after an invite, `room members` must report the
/// invitee with `status=invited`.
#[test]
fn invite_invitee_appears_in_members_as_invited() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .success();

    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=invited"));
}

/// Two successive invites for different invitee keys must produce distinct
/// `invite_id` values and distinct ticket tokens.
#[test]
fn two_invites_produce_distinct_ids_and_tokens() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let invitee_b = "0505050505050505050505050505050505050505050505050505050505050505";

    let out1 = cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .output()
        .unwrap();
    assert!(out1.status.success());

    let out2 = cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", invitee_b])
        .output()
        .unwrap();
    assert!(out2.status.success());

    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);

    let id1 = extract_field(&stdout1, "invite_id").expect("invite_id in first output");
    let id2 = extract_field(&stdout2, "invite_id").expect("invite_id in second output");
    assert_ne!(
        id1, id2,
        "successive invites must produce distinct invite_ids"
    );

    let tok1 = extract_ticket(&stdout1).expect("ticket in first output");
    let tok2 = extract_ticket(&stdout2).expect("ticket in second output");
    assert_ne!(
        tok1, tok2,
        "successive invites must produce distinct ticket tokens"
    );
}

// ── security (AC3): secret seeds never in plaintext output ───────────────────

/// AC3: the raw secret seeds stored in `identity.secret` must never appear in
/// `stdout` or `stderr` of `room invite` — confirming the secret only travels
/// inside the encoded ticket token.
#[test]
fn invite_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret")).unwrap();
    let secret_v: serde_json::Value = serde_json::from_str(&secret_raw).unwrap();
    let identity_seed = secret_v["identity_secret"].as_str().unwrap().to_owned();
    let device_seed = secret_v["device_secret"].as_str().unwrap().to_owned();

    let out = cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stdout.contains(&identity_seed),
        "invite stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(&identity_seed),
        "invite stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(&device_seed),
        "invite stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(&device_seed),
        "invite stderr must not contain the device secret seed"
    );
}

// ── persistence (survives CLI restart) ───────────────────────────────────────

/// An invite issued by one process must be visible to a separate process's
/// `room members` — the event is read from `rooms.db`, not from in-process memory.
#[test]
fn invite_survives_cli_restart_and_appears_in_members() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .success();

    // A fresh `cmd()` call is a separate process — simulates a restart.
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=invited"));
}

// ── error paths — pre-IO validation (writes nothing) ─────────────────────────

/// No prior identity must produce a non-zero exit with an actionable hint
/// pointing at `identity create`.
#[test]
fn invite_without_identity_exits_nonzero_with_hint() {
    let home = TempDir::new().unwrap();
    let fake_room = format!("blake3:{}", "aa".repeat(32));
    cmd(&home)
        .args(["room", "invite", &fake_room, "--invitee", INVITEE_HEX])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("identity create").or(predicate::str::contains("identity")),
        );
}

/// A well-formed but unknown room id must exit non-zero with an actionable
/// "no room" message.
#[test]
fn invite_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // Create one room so rooms.db exists; the unknown id is a different room.
    create_room(&home);

    let unknown_id = format!("blake3:{}", "ab".repeat(32));
    cmd(&home)
        .args(["room", "invite", &unknown_id, "--invitee", INVITEE_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));
}

/// Unknown room: `rooms.db` must be unchanged (the command wrote nothing to it).
#[test]
fn invite_unknown_room_does_not_modify_db() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    create_room(&home);

    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    let unknown_id = format!("blake3:{}", "ab".repeat(32));
    cmd(&home)
        .args(["room", "invite", &unknown_id, "--invitee", INVITEE_HEX])
        .assert()
        .failure();

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when invite is rejected for an unknown room"
    );
}

/// D8: `--role admin` must be rejected before any IO (single immutable admin).
#[test]
fn invite_role_admin_exits_nonzero_before_io() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            INVITEE_HEX,
            "--role",
            "admin",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("admin"));

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when --role admin is rejected"
    );
}

/// An unknown role must be rejected with a non-zero exit.
#[test]
fn invite_unknown_role_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            INVITEE_HEX,
            "--role",
            "superuser",
        ])
        .assert()
        .failure();
}

/// D7: each invalid `--expires` value must be rejected before any IO.
#[test]
fn invite_bad_expires_exits_nonzero_before_io() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    for bad in &["5x", "0h", "12", "abc", "h", "99999999999999999999d"] {
        cmd(&home)
            .args([
                "room",
                "invite",
                &room_id,
                "--invitee",
                INVITEE_HEX,
                "--expires",
                bad,
            ])
            .assert()
            .failure();
    }

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when --expires is invalid (pre-IO gate)"
    );
}

/// D9: a `--invitee` hex that is too short must be rejected before any IO.
#[test]
fn invite_too_short_invitee_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    // 62 hex chars = 31 bytes (one byte short).
    let short = "aa".repeat(31);
    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", &short])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invitee").or(predicate::str::contains("identity")));
}

/// D9: a `--invitee` with non-hex characters must be rejected.
#[test]
fn invite_non_hex_invitee_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    // 64 chars but 'z' is not a valid hex digit.
    let non_hex = "zz".repeat(32);
    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", &non_hex])
        .assert()
        .failure();
}

/// D9: inviting the caller's own identity (self-invite) must be rejected
/// before any IO, with an actionable message.
#[test]
fn invite_self_invite_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let admin_id = get_identity_id(&home);

    cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", &admin_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("yourself").or(predicate::str::contains("self")));
}

/// A syntactically malformed room id (not `blake3:<hex>`) must exit non-zero
/// with an actionable message about the room id format.
#[test]
fn invite_malformed_room_id_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    cmd(&home)
        .args(["room", "invite", "not-a-room-id", "--invitee", INVITEE_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("room id").or(predicate::str::contains("invalid")));
}

/// AC1 (non-admin path): a node whose identity is NOT the room admin but whose
/// store holds the room events (simulated by copying `rooms.db`) must be
/// rejected with an actionable "admin" error, and `rooms.db` must be unchanged.
#[test]
fn invite_non_admin_exits_nonzero_with_actionable_message() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();

    // Alice creates the room (admin in home_a).
    cmd_at(home_a.path())
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();
    let out = cmd_at(home_a.path())
        .args(["room", "create", "Alice's Room"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let room_id = extract_field(&stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();

    // Bob has a fresh identity in home_b.
    cmd_at(home_b.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    // Give Bob the room store: copy Alice's rooms.db into home_b so Bob sees the
    // room events but is NOT the room admin.
    std::fs::copy(
        home_a.path().join("rooms.db"),
        home_b.path().join("rooms.db"),
    )
    .expect("copy rooms.db from home_a to home_b");

    let db_before = std::fs::metadata(home_b.path().join("rooms.db")).map_or(0, |m| m.len());

    // Bob tries to invite → must fail with an "admin" error.
    cmd_at(home_b.path())
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("admin"));

    // rooms.db must be unchanged (no partial write).
    let db_after = std::fs::metadata(home_b.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when invite is rejected for a non-admin caller (AC1)"
    );
}

// ── data-dir isolation ────────────────────────────────────────────────────────

/// AC2 + fold: after an agent-role invite, `room members` must list the
/// invitee with `role=agent`, not the default `role=member`. This crosses
/// the CLI → event-store → membership-fold boundary for the role field.
#[test]
fn invite_agent_role_appears_in_members_as_agent() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            INVITEE_HEX,
            "--role",
            "agent",
        ])
        .assert()
        .success();

    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=agent"))
        .stdout(predicate::str::contains("status=invited"));
}

/// AC2/AC4 ticket-codec boundary: extract the `roomtkt1…` token from
/// `room invite` output, decode it via `RoomInviteTicket::from_str`,
/// and assert that the parsed ticket's invitee key matches the CLI
/// `--invitee` argument exactly. This crosses the CLI ↔ ticket-codec
/// boundary — a contract not tested at the pure core level, which never
/// exercises the CLI's random secret draw and output path.
#[test]
fn invite_ticket_decodes_to_correct_invitee_key() {
    use iroh_rooms_core::ticket::RoomInviteTicket;
    use std::str::FromStr;

    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    let token = extract_ticket(&stdout).expect("ticket token must follow the 'ticket:' label line");
    let ticket =
        RoomInviteTicket::from_str(token).expect("CLI-produced ticket must decode without error");

    // AC2: the decoded invitee key must exactly match the --invitee argument.
    assert_eq!(
        ticket.invitee_key.to_string(),
        INVITEE_HEX,
        "ticket.invitee_key must match the --invitee hex argument"
    );

    // AC4 substrate: the ticket's recomputed capability hash must be
    // self-consistent (BLAKE3 of room ‖ invite ‖ secret == ticket.capability_hash()).
    let recomputed = iroh_rooms_core::event::capability_hash(
        &ticket.room_id,
        &ticket.invite_id,
        &ticket.capability_secret,
    );
    assert_eq!(
        ticket.capability_hash(),
        recomputed,
        "ticket.capability_hash() must equal the standalone derivation (AC4)"
    );
}

/// An invite issued under `--data-dir <A>` must not create or modify anything
/// in `--data-dir <B>`.
#[test]
fn data_dir_flag_isolates_invites() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();

    cmd_at(home_a.path())
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();
    cmd_at(home_b.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    let room_id = {
        let out = cmd_at(home_a.path())
            .args(["room", "create", "Alice's Room"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        extract_field(&stdout, "room_id")
            .expect("room_id in create output")
            .to_owned()
    };

    // Issue an invite in home_a.
    cmd_at(home_a.path())
        .args(["room", "invite", &room_id, "--invitee", INVITEE_HEX])
        .assert()
        .success();

    // home_b must have no rooms.db (no invite or room was created there).
    assert!(
        !home_b.path().join("rooms.db").exists(),
        "rooms.db must NOT appear in home_b after an invite issued in home_a"
    );
}
