//! CLI integration tests for the `agent` noun (IR-0206 §11).
//!
//! `agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]` is a thin,
//! delegating façade over the landed key-bound invite path
//! (`invite::invite(.., "agent", ..)`, IR-0103): no new authorization, no new
//! event type. These tests drive the real binary offline against isolated temp
//! homes and prove the four acceptance criteria at the CLI boundary:
//!
//!   AC1 (own identity + device key) — `agent_identity_has_distinct_keys`
//!                                     + `agent_and_human_identities_are_distinct`
//!   AC2 (agent role in membership)  — `agent_invite_exits_zero_with_agent_role`
//!                                     + `agent_invite_appears_in_members_as_agent`
//!                                     + `agent_invite_ticket_decodes_with_agent_role`
//!   AC3 (no implicit access)        — `agent_invite_by_non_admin_is_rejected`
//!   AC4 (one shared protocol model) — `agent_invite_matches_room_invite_role_agent`
//!   delegation / error parity       — self-invite, bad agent id, bad expires,
//!                                     unknown room, malformed room id, no identity,
//!                                     the hard-coded (flagless) role, `--expires`
//!                                     passthrough, secret hygiene, `--data-dir`
//!                                     isolation.
//!
//! The online agent-*join* half (agent redeems its ticket, then appears
//! `role: agent, status: active`) follows the `two_peer_e2e.rs` `#[ignore]`
//! loopback convention and lives in the e2e phase; here the always-green offline
//! half (invite → `status: invited`) is exercised end to end.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ── helpers (mirrors tests/invite_cli.rs) ──────────────────────────────────────

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
fn create_identity_named(home: &TempDir, name: &str) {
    cmd(home)
        .args(["identity", "create", "--name", name])
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

/// A fixed 64-hex-char agent identity key (32 raw bytes). `IdentityKey::from_bytes`
/// does not validate curve-point membership, so any well-formed 32-byte hex works
/// for invite tests; this value differs from any CSPRNG-generated admin key.
const AGENT_HEX: &str = "0606060606060606060606060606060606060606060606060606060606060606";
/// A second, distinct agent key for the AC4 equivalence test.
const AGENT_HEX_B: &str = "0707070707070707070707070707070707070707070707070707070707070707";

// ── AC1: an agent has its own identity and device key ──────────────────────────

/// AC1: an agent identity created with the same `identity create` path a human
/// uses (spike §1: an agent is an ordinary principal) exposes a 64-hex
/// `identity_id` and a **distinct** 64-hex `device_id`.
#[test]
fn agent_identity_has_distinct_keys() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "build-agent");

    let out = cmd(&home)
        .args(["identity", "show", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let identity_id = v["identity_id"].as_str().expect("identity_id present");
    let device_id = v["device_id"].as_str().expect("device_id present");

    assert_eq!(identity_id.len(), 64, "identity_id must be 64-hex");
    assert_eq!(device_id.len(), 64, "device_id must be 64-hex");
    assert!(
        identity_id.chars().all(|c| c.is_ascii_hexdigit()),
        "identity_id must be lowercase hex"
    );
    assert!(
        device_id.chars().all(|c| c.is_ascii_hexdigit()),
        "device_id must be lowercase hex"
    );
    assert_ne!(
        identity_id, device_id,
        "an agent's identity and device keys must be distinct (AC1)"
    );
}

/// AC1 / AC4: an agent and a human, both created via `identity create`, are
/// distinct principals — different `identity_id`s — proving the agent is not a
/// shared or derived key but a first-class participant on the same identity path.
#[test]
fn agent_and_human_identities_are_distinct() {
    let agent_home = TempDir::new().unwrap();
    let human_home = TempDir::new().unwrap();
    create_identity_named(&agent_home, "build-agent");
    create_identity_named(&human_home, "Alice");

    assert_ne!(
        get_identity_id(&agent_home),
        get_identity_id(&human_home),
        "an agent and a human identity must be distinct principals (AC1)"
    );
}

// ── AC2: the agent role appears in membership state ────────────────────────────

/// AC2 surface: an admin's `agent invite` exits 0 and prints `role: agent` plus
/// a `roomtkt1…` ticket — the agent is authorized *as an agent*.
#[test]
fn agent_invite_exits_zero_with_agent_role() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success(), "admin agent invite must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        extract_field(&stdout, "role"),
        Some("agent"),
        "`agent invite` must report role: agent"
    );
    assert_eq!(
        extract_field(&stdout, "invitee"),
        Some(AGENT_HEX),
        "the invitee line must echo the bound agent key (AC2 key-binding)"
    );
    let token = extract_ticket(&stdout).expect("a ticket line must follow 'ticket:'");
    assert!(
        token.starts_with("roomtkt1"),
        "ticket token must start with 'roomtkt1', got: {token}"
    );
}

/// AC2 fold integration: after `agent invite`, the membership view re-derived from
/// the persisted log lists the agent with `role=agent` and `status=invited`, in
/// both the labeled and `--json` renderings.
#[test]
fn agent_invite_appears_in_members_as_agent() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .success();

    // Labeled view — a separate process reads rooms.db (offline fold).
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=agent"))
        .stdout(predicate::str::contains("status=invited"))
        .stdout(predicate::str::contains(AGENT_HEX));

    // JSON view — the role field must serialize as "agent" too.
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("members --json is JSON");
    let json = v.to_string();
    assert!(
        json.contains("\"role\":\"agent\"") || json.contains("\"agent\""),
        "members --json must surface the agent role: {json}"
    );
}

/// AC2 ticket-codec boundary: decode the `roomtkt1…` token from `agent invite`
/// and assert the ticket carries `role == "agent"` bound to the exact agent key —
/// crossing the CLI ↔ ticket-codec boundary the pure core never exercises.
#[test]
fn agent_invite_ticket_decodes_with_agent_role() {
    use iroh_rooms_core::ticket::RoomInviteTicket;
    use std::str::FromStr;

    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    let token = extract_ticket(&stdout).expect("ticket token must follow 'ticket:'");
    let ticket = RoomInviteTicket::from_str(token).expect("CLI ticket must decode");

    assert_eq!(
        ticket.role, "agent",
        "decoded ticket must carry role agent (AC2)"
    );
    assert_eq!(
        ticket.invitee_key.to_string(),
        AGENT_HEX,
        "decoded ticket must be key-bound to the agent id (AC2)"
    );
}

/// AC2 durability: an agent invite issued by one process is visible to a separate
/// `room members` process — read from rooms.db, not in-process memory.
#[test]
fn agent_invite_survives_cli_restart() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .success();

    // A fresh `cmd()` is a distinct process — simulates a restart.
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=agent"))
        .stdout(predicate::str::contains("status=invited"));
}

// ── AC3: an agent cannot access a room without an explicit (admin) invite ───────

/// AC3 (authorization at the CLI boundary): only the room admin may authorize an
/// agent. A non-admin caller who *has the room events* (store copied in) but is
/// not the admin cannot mint an agent invite — the request fails with an
/// actionable "admin" error and rooms.db is left byte-for-byte unchanged, so no
/// agent is granted access without the admin's signature.
#[test]
fn agent_invite_by_non_admin_is_rejected() {
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

    // Bob has a fresh identity and a copy of the room store, but is NOT the admin.
    cmd_at(home_b.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();
    std::fs::copy(
        home_a.path().join("rooms.db"),
        home_b.path().join("rooms.db"),
    )
    .expect("copy rooms.db into home_b");
    let db_before = std::fs::metadata(home_b.path().join("rooms.db")).map_or(0, |m| m.len());

    // Bob tries to invite an agent → rejected (agents cannot be admitted except by
    // the single immutable admin; an agent has no implicit path in).
    cmd_at(home_b.path())
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("admin"));

    let db_after = std::fs::metadata(home_b.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must be unchanged when a non-admin's agent invite is rejected (AC3)"
    );
}

// ── AC4: human and agent are represented through one protocol model ─────────────

/// AC4: `agent invite <ROOM> <ID>` and `room invite <ROOM> --invitee <ID> --role
/// agent` are the same operation. Issued in one room for two agent keys, both
/// render identically in membership state (`role=agent status=invited`), and both
/// tickets decode to `role == "agent"` — the agent noun is pure surface over the
/// shared `member.invited` model, not a distinct principal type.
#[test]
fn agent_invite_matches_room_invite_role_agent() {
    use iroh_rooms_core::ticket::RoomInviteTicket;
    use std::str::FromStr;

    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    // Path 1: the `agent` noun.
    let out_noun = cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .output()
        .unwrap();
    assert!(out_noun.status.success());
    let noun_stdout = String::from_utf8_lossy(&out_noun.stdout);

    // Path 2: `room invite --role agent` for a different agent key, same room.
    let out_flag = cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            AGENT_HEX_B,
            "--role",
            "agent",
        ])
        .output()
        .unwrap();
    assert!(out_flag.status.success());
    let flag_stdout = String::from_utf8_lossy(&out_flag.stdout);

    // Both output surfaces report the same role.
    assert_eq!(extract_field(&noun_stdout, "role"), Some("agent"));
    assert_eq!(extract_field(&flag_stdout, "role"), Some("agent"));

    // Both tickets decode to the same role, differing only in the bound key.
    let t_noun = RoomInviteTicket::from_str(extract_ticket(&noun_stdout).unwrap()).unwrap();
    let t_flag = RoomInviteTicket::from_str(extract_ticket(&flag_stdout).unwrap()).unwrap();
    assert_eq!(t_noun.role, "agent");
    assert_eq!(
        t_flag.role, t_noun.role,
        "both paths mint the identical role"
    );
    assert_eq!(t_noun.invitee_key.to_string(), AGENT_HEX);
    assert_eq!(t_flag.invitee_key.to_string(), AGENT_HEX_B);

    // Both agents land in membership state with the identical shape: one
    // `role=agent` line per key (exactly two, since the admin is role=admin).
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains(AGENT_HEX))
        .stdout(predicate::str::contains(AGENT_HEX_B))
        .stdout(predicate::str::contains("role=agent").count(2));
}

/// AC4 (persisted-event equivalence, risk R1): reading the log back offline, the
/// `member.invited` event written by `agent invite` and the one written by `room
/// invite --role agent` are structurally identical — same `event_type`, same
/// `invited_role: agent` — differing only in the bound `invitee` key. This crosses
/// the CLI→store→re-fold→JSON boundary the ticket/members assertions never touch,
/// proving the agent noun mints a byte-shape-identical event, not a distinct one.
#[test]
fn agent_invite_persists_same_member_invited_as_room_invite() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    // Path 1: the agent noun. Path 2: `room invite --role agent`, distinct key.
    cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .success();
    cmd(&home)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            AGENT_HEX_B,
            "--role",
            "agent",
        ])
        .assert()
        .success();

    // A separate process reads the persisted timeline back as JSON (offline fold).
    let out = cmd(&home)
        .args(["room", "tail", &room_id, "--offline", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "offline tail --json must succeed");
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("tail --json emits a JSON array");
    let rows = value.as_array().expect("tail --json is an array");

    let invites: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| r["event_type"] == "member.invited")
        .collect();
    assert_eq!(
        invites.len(),
        2,
        "exactly two member.invited events were persisted, got: {value}"
    );

    // Both persisted invites carry the identical role attribution. (The tail row
    // flattens the content fields to the top level, so `invited_role`/`invitee`
    // are direct keys.)
    for row in &invites {
        assert_eq!(
            row["invited_role"], "agent",
            "every persisted invite must record invited_role=agent: {row}"
        );
    }

    // …and differ only in the bound invitee key (06… from the noun, 07… from --role).
    let mut bound_keys: Vec<String> = invites
        .iter()
        .map(|r| {
            r["invitee"]
                .as_str()
                .expect("each member.invited row carries an invitee")
                .to_owned()
        })
        .collect();
    bound_keys.sort();
    assert_eq!(
        bound_keys,
        vec![AGENT_HEX.to_owned(), AGENT_HEX_B.to_owned()],
        "the two invites bind the two distinct agent keys"
    );
}

// ── delegation & error parity (agent invite inherits invite::invite verbatim) ──

/// `agent invite` exposes **no** `--role` flag: the role is hard-coded to `agent`
/// (D1/D2). A stray `--role` must be rejected at the argument-parse layer.
#[test]
fn agent_invite_has_no_role_flag() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, AGENT_HEX, "--role", "member"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--role").or(predicate::str::contains("unexpected")));
}

/// `--expires` is passed through the wrapper: `agent invite … --expires 24h`
/// renders the absolute ISO-8601 instant and the `(in 24h)` annotation, exactly
/// as `room invite` does.
#[test]
fn agent_invite_expires_is_passed_through() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
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
        expires_line.contains("(in 24h)"),
        "expiry must be echoed as (in 24h): {expires_line}"
    );
    assert!(
        expires_line.contains('T') && expires_line.contains('Z'),
        "expiry must render an ISO-8601 UTC instant: {expires_line}"
    );
}

/// Self-invite (agent id == the caller's own identity) is rejected before any IO —
/// the wrapper delegates the self-invite guard unchanged.
#[test]
fn agent_invite_self_invite_is_rejected() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);
    let own_id = get_identity_id(&home);

    cmd(&home)
        .args(["agent", "invite", &room_id, &own_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("yourself").or(predicate::str::contains("self")));
}

/// A malformed agent id (too short / non-hex) is rejected before any IO and
/// leaves rooms.db unchanged — inherited `--invitee` parsing.
#[test]
fn agent_invite_bad_agent_id_is_rejected_before_io() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    for bad in [&"aa".repeat(31)[..], &"zz".repeat(32)[..]] {
        cmd(&home)
            .args(["agent", "invite", &room_id, bad])
            .assert()
            .failure();
    }

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change when the agent id is invalid (pre-IO gate)"
    );
}

/// Each invalid `--expires` value is rejected before any IO and leaves rooms.db
/// unchanged — inherited expiry parsing.
#[test]
fn agent_invite_bad_expires_is_rejected_before_io() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    let room_id = create_room(&home);
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    for bad in &["5x", "0h", "12", "abc", "h", "99999999999999999999d"] {
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

/// A well-formed but unknown room id exits non-zero with a "no room" message and
/// leaves the store unchanged.
#[test]
fn agent_invite_unknown_room_is_rejected() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
    create_room(&home); // so rooms.db exists; the unknown id is a different room
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    let unknown = format!("blake3:{}", "ab".repeat(32));
    cmd(&home)
        .args(["agent", "invite", &unknown, AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must not change for an unknown room"
    );
}

/// A syntactically malformed room id (not `blake3:<hex>`) is rejected with an
/// actionable message.
#[test]
fn agent_invite_malformed_room_id_is_rejected() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");

    cmd(&home)
        .args(["agent", "invite", "not-a-room-id", AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("room id").or(predicate::str::contains("invalid")));
}

/// No prior identity → non-zero exit with an `identity`-pointing hint.
#[test]
fn agent_invite_without_identity_is_rejected() {
    let home = TempDir::new().unwrap();
    let fake_room = format!("blake3:{}", "aa".repeat(32));

    cmd(&home)
        .args(["agent", "invite", &fake_room, AGENT_HEX])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identity"));
}

// ── secret hygiene & isolation ─────────────────────────────────────────────────

/// The raw secret seeds in `identity.secret` must never appear in `agent invite`
/// stdout/stderr — the capability secret travels only inside the ticket token.
#[test]
fn agent_invite_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity_named(&home, "Alice");
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

    for (label, seed) in [("identity", &identity_seed), ("device", &device_seed)] {
        assert!(
            !stdout.contains(seed.as_str()),
            "agent invite stdout must not contain the {label} secret seed"
        );
        assert!(
            !stderr.contains(seed.as_str()),
            "agent invite stderr must not contain the {label} secret seed"
        );
    }
}

/// An agent invite issued under `--data-dir <A>` must not create or modify
/// anything under `--data-dir <B>`.
#[test]
fn agent_invite_data_dir_isolation() {
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

    cmd_at(home_a.path())
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        .success();

    assert!(
        !home_b.path().join("rooms.db").exists(),
        "rooms.db must NOT appear in home_b after an agent invite issued in home_a"
    );
}
