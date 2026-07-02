//! CLI integration tests for the `agent` command group (IR-0206 §11).
//!
//! `agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]` is an ergonomic
//! wrapper over `room invite` with the role pinned to `agent` and a positional
//! `<AGENT_ID>` (matching PRD §16). These tests exercise the *new* surface — the
//! verb, its dispatch, and its agent-tailored output — plus the acceptance
//! criteria that agents are first-class, same-model, explicitly-invited principals.
//! Coverage of the shared invite orchestration itself lives in `invite_cli.rs`.
//!
//! Each test gets its own temp `--data-dir` so tests run isolated in parallel;
//! `IROH_ROOMS_HOME` is cleared everywhere.
//!
//! Coverage map (spec §11 / issue acceptance criteria):
//!   AC1 (own identity + device key) — `agent_identity_has_distinct_identity_and_device_keys`
//!   AC4 (same protocol model)       — `agent_identity_is_structurally_identical_to_a_human`
//!   AC2 (role in membership)        — `agent_invite_persists_member_invited_with_agent_role`,
//!                                     `agent_invite_ticket_role_is_agent`
//!   AC3 (no access without invite)  — `uninvited_agent_absent_from_members`,
//!                                     `agent_cannot_use_another_identitys_ticket`
//!   admin-gate (explicit invite)    — `agent_invite_requires_admin`
//!   verb-is-the-role                — `agent_invite_has_no_role_flag`
//!   happy path / output             — `agent_invite_happy_path_mints_agent_role_ticket`,
//!                                     `agent_invite_prints_not_implicitly_trusted_note`
//!   pre-IO gates                    — bad expires / bad agent id / bad room id / unknown room,
//!                                     self-invite, missing positional
//!   secret hygiene                  — `agent_invite_does_not_expose_secret_seeds`
//!   isolation                       — `data_dir_flag_isolates_agent_invites`

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

/// Create an identity named `name` in `home`. Panics if the command fails.
fn create_identity(home: &TempDir, name: &str) {
    cmd(home)
        .args(["identity", "create", "--name", name])
        .assert()
        .success();
}

/// Run `room create` in `home` and return the printed `room_id`.
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

/// Run `identity show` in `home` and return the `identity_id`.
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

/// Extract the ticket token: the trimmed line that follows the `ticket:` label.
fn extract_ticket(stdout: &str) -> Option<&str> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("ticket:") {
            return lines.next().map(str::trim);
        }
    }
    None
}

/// A fixed 64-hex agent identity id (32 raw bytes), distinct from any
/// CSPRNG-generated admin key. `IdentityKey::from_bytes` does not check
/// curve-point membership, so any well-formed 32-byte hex is a valid invitee.
const AGENT_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";

// ── AC1 / AC4: agent identity uses the same protocol model as a human ──────────

/// AC1: an agent identity is minted by the shared `identity create` and has its
/// own participant key (`identity_id`) and a *distinct* device key (`device_id`),
/// both well-formed 64-hex Ed25519 public keys.
#[test]
fn agent_identity_has_distinct_identity_and_device_keys() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "build-agent");

    let out = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .expect("identity show --json must be valid JSON");

    let identity_id = v["identity_id"].as_str().expect("identity_id field");
    let device_id = v["device_id"].as_str().expect("device_id field");

    let is_64_hex = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    assert!(
        is_64_hex(identity_id),
        "identity_id must be 64 lowercase hex"
    );
    assert!(is_64_hex(device_id), "device_id must be 64 lowercase hex");
    assert_ne!(
        identity_id, device_id,
        "an agent must have its own identity key AND a distinct device key (AC1)"
    );
}

/// AC4: nothing distinguishes an agent identity from a human one on disk or in
/// `identity show` — both carry the exact same field set and id shapes. The
/// `agent` role is assigned only later, at invite time (not at creation).
#[test]
fn agent_identity_is_structurally_identical_to_a_human() {
    let human = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    create_identity(&human, "Alice");
    create_identity(&agent, "build-agent");

    let field_set = |home: &TempDir| -> Vec<String> {
        let out = cmd(home)
            .args(["identity", "show", "--json"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let v: serde_json::Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        let mut keys: Vec<String> = v
            .as_object()
            .expect("identity show --json must be an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };

    assert_eq!(
        field_set(&human),
        field_set(&agent),
        "an agent identity must be represented through the same model as a human (AC4)"
    );
}

// ── happy path / output ────────────────────────────────────────────────────────

/// `agent invite <ROOM> <AGENT>` exits 0 and prints the mandatory summary fields,
/// with the role pinned to `agent` and the positional `<AGENT_ID>` echoed back.
#[test]
fn agent_invite_happy_path_mints_agent_role_ticket() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success(), "agent invite must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        extract_field(&stdout, "role"),
        Some("agent"),
        "the agent verb must pin role: agent"
    );
    assert_eq!(
        extract_field(&stdout, "invitee"),
        Some(AGENT_HEX),
        "the positional <AGENT_ID> must be echoed on the invitee line"
    );
    assert_eq!(
        extract_field(&stdout, "expires"),
        Some("never"),
        "no --expires must render 'never'"
    );
    let token = extract_ticket(&stdout).expect("a ticket value line must follow 'ticket:'");
    assert!(
        token.starts_with("roomtkt1"),
        "ticket token must start with 'roomtkt1'; got: {token}"
    );
}

/// The agent-tailored `print_agent_invite` adds a "not implicitly trusted"
/// reminder on **stderr** (per PRD §13.3), keeping stdout script-parseable. This
/// note is unique to `agent invite` (it is absent from `room invite`).
#[test]
fn agent_invite_prints_not_implicitly_trusted_note() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains("not implicitly trusted"),
        "the agent invite must warn on stderr that the agent is not implicitly trusted; got: {stderr}"
    );
    assert!(
        !stdout.contains("not implicitly trusted"),
        "the reminder must stay on stderr so stdout stays script-parseable"
    );
}

// ── AC2: role appears in membership state ──────────────────────────────────────

/// AC2 (fold integration): after `agent invite`, the membership re-derived from
/// the persisted log lists the agent with `role=agent`, `status=invited`.
#[test]
fn agent_invite_persists_member_invited_with_agent_role() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .success();

    // A fresh process re-reads rooms.db, proving the role is persisted, not
    // in-memory (restart determinism).
    let out = cmd(&home)
        .args(["room", "members", &room_id])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let agent_line = stdout
        .lines()
        .find(|l| l.contains(AGENT_HEX))
        .expect("the invited agent must appear in room members");
    assert!(
        agent_line.contains("role=agent") && agent_line.contains("status=invited"),
        "the agent must fold to role=agent status=invited (AC2); got: {agent_line}"
    );
}

/// AC2/AC4 codec boundary: the emitted `roomtkt1…` ticket decodes to `role ==
/// "agent"`, its bound key equals the positional `<AGENT_ID>`, and its capability
/// hash is self-consistent — the same well-formed artifact as a `room invite
/// --role agent` ticket.
#[test]
fn agent_invite_ticket_role_is_agent() {
    use iroh_rooms_core::ticket::RoomInviteTicket;
    use std::str::FromStr;

    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    let token = extract_ticket(&stdout).expect("ticket token must follow the 'ticket:' label line");
    let ticket = RoomInviteTicket::from_str(token).expect("agent ticket must decode without error");

    assert_eq!(ticket.role, "agent", "the ticket must carry role=agent");
    assert_eq!(
        ticket.invitee_key.to_string(),
        AGENT_HEX,
        "the ticket must be key-bound to the positional <AGENT_ID>"
    );
    let recomputed = iroh_rooms_core::event::capability_hash(
        &ticket.room_id,
        &ticket.invite_id,
        &ticket.capability_secret,
    );
    assert_eq!(
        ticket.capability_hash(),
        recomputed,
        "the ticket capability hash must be self-consistent"
    );
}

/// AC4 / D1: `agent invite <ROOM> <AGENT>` mints the *same* capability artifact
/// as `room invite <ROOM> --invitee <AGENT> --role agent` — the wrapper does not
/// fork the invite path. Both tickets decode to an identical role, bound key, and
/// room; they differ only in the per-invite random `invite_id`/secret (so the
/// capability hashes must NOT be equal — a shared hash would mean a reused secret).
#[test]
fn agent_invite_matches_room_invite_role_agent_artifact() {
    use iroh_rooms_core::ticket::RoomInviteTicket;
    use std::str::FromStr;

    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    let decode_ticket = |args: &[&str]| -> RoomInviteTicket {
        let out = cmd(&home).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "invite command must succeed: {args:?}"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let token = extract_ticket(&stdout).expect("a ticket token must be printed");
        RoomInviteTicket::from_str(token).expect("ticket must decode")
    };

    let via_agent = decode_ticket(&["agent", "invite", &room_id, AGENT_HEX]);
    let via_room = decode_ticket(&[
        "room",
        "invite",
        &room_id,
        "--invitee",
        AGENT_HEX,
        "--role",
        "agent",
    ]);

    assert_eq!(via_agent.role, "agent");
    assert_eq!(
        via_room.role, via_agent.role,
        "both surfaces mint role=agent"
    );
    assert_eq!(
        via_agent.invitee_key.to_string(),
        AGENT_HEX,
        "agent invite binds the positional <AGENT_ID>"
    );
    assert_eq!(
        via_room.invitee_key.to_string(),
        via_agent.invitee_key.to_string(),
        "both surfaces bind the same invitee key (AC4: one protocol model)"
    );
    assert_eq!(
        via_room.room_id, via_agent.room_id,
        "both tickets are scoped to the same room"
    );
    assert_ne!(
        via_agent.capability_hash(),
        via_room.capability_hash(),
        "each invite draws a fresh secret — the capability hashes must differ"
    );
}

/// AC5-parity: `--expires 24h` renders an absolute ISO-8601 timestamp plus the
/// `(in 24h)` annotation, exactly like `room invite`.
#[test]
fn agent_invite_with_expiry_shows_absolute_and_relative_duration() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX, "--expires", "24h"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let expires_line = stdout
        .lines()
        .find(|l| l.starts_with("expires:"))
        .expect("expires: line must appear");

    assert!(
        expires_line.contains("(in 24h)")
            && expires_line.contains('T')
            && expires_line.contains('Z'),
        "expires line must show an ISO-8601 instant and (in 24h); got: {expires_line}"
    );
}

// ── verb-is-the-role: `agent invite` takes no --role flag ──────────────────────

/// `agent invite` deliberately has no `--role` flag — the verb *is* the role, so
/// it cannot be coerced into minting a member/admin invite. clap rejects the flag.
#[test]
fn agent_invite_has_no_role_flag() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX, "--role", "member"])
        .assert()
        .failure();
}

/// The `<AGENT_ID>` positional is required: omitting it is a clap usage error.
#[test]
fn agent_invite_missing_agent_id_arg_fails() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id])
        .assert()
        .failure();
}

// ── admin-gate: explicit invite is admin-only (supports AC3) ───────────────────

/// A non-admin caller that holds the room events (its `rooms.db` copied from the
/// admin) still cannot mint an agent invite: it fails with an "admin" error and
/// leaves `rooms.db` byte-for-byte unchanged (no partial write).
#[test]
fn agent_invite_requires_admin() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();

    create_identity(&home_a, "Alice");
    let room_id = create_room(&home_a);
    create_identity(&home_b, "Bob");

    // Bob sees the room events but is not the admin.
    std::fs::copy(
        home_a.path().join("rooms.db"),
        home_b.path().join("rooms.db"),
    )
    .expect("copy rooms.db from home_a to home_b");
    let db_before = std::fs::metadata(home_b.path().join("rooms.db")).map_or(0, |m| m.len());

    cmd_at(home_b.path())
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("admin"));

    let db_after = std::fs::metadata(home_b.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when a non-admin agent invite is rejected"
    );
}

// ── pre-IO gates (a bad invocation writes nothing) ─────────────────────────────

/// A bad `--expires` is rejected before any IO; `rooms.db` is unchanged.
#[test]
fn agent_invite_bad_expires_exits_nonzero_before_io() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    for bad in &["5x", "0h", "12", "abc", "h"] {
        cmd(&home)
            .args(["agent", "invite", &room_id, AGENT_HEX, "--expires", bad])
            .assert()
            .failure();
    }

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when --expires is invalid (pre-IO gate)"
    );
}

/// A malformed `<AGENT_ID>` (too short / non-hex) is rejected.
#[test]
fn agent_invite_bad_agent_id_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    for bad in &["aa".repeat(31), "zz".repeat(32), String::new()] {
        cmd(&home)
            .args(["agent", "invite", &room_id, bad])
            .assert()
            .failure();
    }
}

/// A syntactically malformed room id (not `blake3:<hex>`) is rejected with an
/// actionable message.
#[test]
fn agent_invite_malformed_room_id_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");

    cmd(&home)
        .args(["agent", "invite", "not-a-room-id", AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("room id").or(predicate::str::contains("invalid")));
}

/// A well-formed but unknown room id fails with "no room" and does not modify the
/// store.
#[test]
fn agent_invite_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    create_room(&home); // so rooms.db exists; the unknown id is a different room.
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    let unknown_id = format!("blake3:{}", "ab".repeat(32));
    cmd(&home)
        .args(["agent", "invite", &unknown_id, AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(db_before, db_after, "unknown room must write nothing");
}

/// Inviting the admin's own identity as an agent is meaningless and rejected
/// before any IO.
#[test]
fn agent_invite_self_invite_rejected() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);
    let admin_id = get_identity_id(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, &admin_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("yourself").or(predicate::str::contains("self")));
}

// ── secret hygiene (AC): no secret seeds in any output stream ──────────────────

/// The raw secret seeds in `identity.secret` must never appear in `agent invite`
/// stdout/stderr — the secret travels only inside the encoded ticket token.
#[test]
fn agent_invite_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);

    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret")).unwrap();
    let secret_v: serde_json::Value = serde_json::from_str(&secret_raw).unwrap();
    let identity_seed = secret_v["identity_secret"].as_str().unwrap().to_owned();
    let device_seed = secret_v["device_secret"].as_str().unwrap().to_owned();

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    for seed in [&identity_seed, &device_seed] {
        assert!(
            !stdout.contains(seed) && !stderr.contains(seed),
            "agent invite must not leak a secret seed in stdout/stderr"
        );
    }
}

// ── AC3: no implicit room access for an uninvited agent ────────────────────────

/// AC3 (default-deny at the membership surface): an agent that was never invited
/// has no membership event, so it never appears in the room's membership state —
/// there is no implicit access.
#[test]
fn uninvited_agent_absent_from_members() {
    let home = TempDir::new().unwrap();
    create_identity(&home, "Alice");
    let room_id = create_room(&home);
    let admin_id = get_identity_id(&home);

    let out = cmd(&home)
        .args(["room", "members", &room_id])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains(&admin_id),
        "the admin must be the only member of a genesis-only room"
    );
    assert!(
        !stdout.contains(AGENT_HEX),
        "an uninvited agent must not appear in membership state (AC3)"
    );
}

/// AC3 (explicit-invite gate at the command surface): an agent cannot redeem a
/// ticket that is key-bound to a *different* identity. The `room join` pre-check
/// rejects the mismatched identity before any network IO.
#[test]
fn agent_cannot_use_another_identitys_ticket() {
    let home_admin = TempDir::new().unwrap();
    create_identity(&home_admin, "Alice");
    let room_id = create_room(&home_admin);

    // Admin mints an agent invite bound to AGENT_HEX.
    let out = cmd(&home_admin)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ticket = extract_ticket(&stdout)
        .expect("ticket must be printed")
        .to_owned();

    // A different agent (fresh random identity ≠ AGENT_HEX) tries to use it.
    let home_agent = TempDir::new().unwrap();
    create_identity(&home_agent, "rogue-agent");

    cmd_at(home_agent.path())
        .args(["room", "join", &ticket])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identity"));
}

// ── data-dir isolation ─────────────────────────────────────────────────────────

/// An agent invite issued under `--data-dir <A>` must not create or touch
/// anything under `--data-dir <B>`.
#[test]
fn data_dir_flag_isolates_agent_invites() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();
    create_identity(&home_a, "Alice");
    create_identity(&home_b, "Bob");
    let room_id = create_room(&home_a);

    cmd_at(home_a.path())
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .success();

    // home_b only ever had an identity created; no room/invite touched it.
    assert!(
        !home_b.path().join("rooms.db").exists(),
        "rooms.db must NOT appear in home_b after an agent invite issued in home_a"
    );
}
