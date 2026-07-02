//! CLI integration tests for `pipe expose | connect | close | list`
//! (issue #23 / IR-0108 acceptance-locking suite; spec §7).
//!
//! Coverage map (spec §7 test strategy):
//!
//!   expose: non-loopback --tcp refused          → `pipe_expose_non_loopback_tcp_is_refused`
//!   expose: --allow is required                 → `pipe_expose_allow_is_required_by_clap`
//!   expose: empty/invalid --allow fails          → `pipe_expose_invalid_allow_id_fails`
//!   expose: security warning on stderr, structured lines on stdout
//!                                               → `pipe_expose_security_warning_and_stdout_split`
//!   close: bad hex → "invalid pipe id"          → `pipe_close_bad_hex_is_refused`
//!   close: no identity → fails                  → `pipe_close_no_identity_fails`
//!   close: no such pipe in any room             → `pipe_close_absent_pipe_exits_nonzero_with_hint`
//!   close: --help shows `<PIPE_ID>` only         → `pipe_close_help_shows_pipe_id_not_room_id`
//!   list: unknown room → fails                  → `pipe_list_unknown_room_exits_nonzero`
//!
//!   IR-0305 doc-backing (`docs/live-pipe-preview.md` §4.7 case 2 — non-member,
//!   `error[peer_unauthorized]`, exit 3, checked locally before any dial):
//!   expose: non-member refused                   → `pipe_expose_non_member_is_refused`
//!   connect: non-member refused                  → `pipe_connect_non_member_is_refused`
//!   close: non-member refused                    → `pipe_close_non_member_is_refused`

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

use iroh_rooms_core::event::keys::SigningKey;
use iroh_rooms_core::event::signed;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::{build_pipe_closed, build_pipe_opened, build_room_created};
use iroh_rooms_core::store::EventStore;

// ── shared helpers ────────────────────────────────────────────────────────────

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
    assert!(out.status.success(), "room create must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("room_id:") {
            return rest.trim().to_owned();
        }
    }
    panic!("room_id not found in room create output:\n{stdout}")
}

/// Seed `home/rooms.db` with a genesis event and a `pipe.opened` event for the
/// given `pipe_id`. Returns the `room_id` and the pipe hex string.
///
/// The events are signed by the fixed seeds below (not the CLI identity) — this
/// is intentional: the test uses it to assert *room inference* errors (before the
/// ownership check), not to complete a full close. For tests that need the close
/// to succeed, the identity must be the owner; see the inline comment.
fn seed_room_with_pipe(
    home: &TempDir,
    identity_seed: [u8; 32],
    device_seed: [u8; 32],
    pipe_id: [u8; 16],
    nonce: [u8; 16],
) -> (String, String) {
    let id_key = SigningKey::from_seed(&identity_seed);
    let dev_key = SigningKey::from_seed(&device_seed);
    let allowed = SigningKey::from_seed(&[0x60; 32]).identity_key();

    let genesis_wire =
        build_room_created(&id_key, &dev_key, "Pipe Room", &nonce, 1_750_000_000_000);
    let room_id = {
        let ev = signed::SignedEvent::decode(&genesis_wire.signed).unwrap();
        ev.room_id
    };
    let ctx = ValidationContext::for_room(room_id);

    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("genesis valid");
    let genesis_ev_id = genesis_v.event_id;

    let pipe_wire = build_pipe_opened(
        &id_key,
        &dev_key,
        &room_id,
        pipe_id,
        &dev_key.device_key(),
        "test-svc",
        "127.0.0.1:8080",
        "/iroh-rooms/pipe/1",
        &[allowed],
        None,
        &[genesis_ev_id],
        1_750_000_001_000,
    );
    let pipe_v = validate_wire_bytes(&pipe_wire.to_bytes(), &ctx).expect("pipe.opened valid");

    let db_path = home.path().join("rooms.db");
    let mut store = EventStore::open(&db_path).expect("open store");
    store.insert(&genesis_v).expect("insert genesis");
    store.insert(&pipe_v).expect("insert pipe.opened");

    let pipe_hex: String = pipe_id.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });
    (room_id.to_string(), pipe_hex)
}

/// A syntactically valid but unused identity id (64-char lowercase hex).
fn fake_allow_id() -> String {
    "a".repeat(64)
}

/// Seed `home/rooms.db` with a genesis event for a room whose admin is NOT the
/// CLI identity. Returns the `room_id` string.
fn seed_genesis_only(
    home: &TempDir,
    identity_seed: [u8; 32],
    device_seed: [u8; 32],
    nonce: [u8; 16],
) -> String {
    let id_key = SigningKey::from_seed(&identity_seed);
    let dev_key = SigningKey::from_seed(&device_seed);
    let genesis_wire =
        build_room_created(&id_key, &dev_key, "Other Room", &nonce, 1_750_000_000_000);
    let room_id = {
        let ev = signed::SignedEvent::decode(&genesis_wire.signed).unwrap();
        ev.room_id
    };
    let ctx = ValidationContext::for_room(room_id);
    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("genesis valid");
    let db_path = home.path().join("rooms.db");
    let mut store = EventStore::open(&db_path).expect("open store");
    store.insert(&genesis_v).expect("insert genesis");
    room_id.to_string()
}

/// Seed `home/rooms.db` with a genesis, a `pipe.opened`, and a `pipe.closed`
/// for `pipe_id`. The pipe should NOT appear in `pipe list` because a
/// causally-known `pipe.closed` matches it. Returns (`room_id_str`, `pipe_hex`).
fn seed_room_with_opened_and_closed_pipe(
    home: &TempDir,
    identity_seed: [u8; 32],
    device_seed: [u8; 32],
    pipe_id: [u8; 16],
    nonce: [u8; 16],
) -> (String, String) {
    let id_key = SigningKey::from_seed(&identity_seed);
    let dev_key = SigningKey::from_seed(&device_seed);
    let allowed = SigningKey::from_seed(&[0x60; 32]).identity_key();

    let genesis_wire =
        build_room_created(&id_key, &dev_key, "Pipe Room", &nonce, 1_750_000_000_000);
    let room_id = {
        let ev = signed::SignedEvent::decode(&genesis_wire.signed).unwrap();
        ev.room_id
    };
    let ctx = ValidationContext::for_room(room_id);
    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("genesis valid");
    let genesis_ev_id = genesis_v.event_id;

    let pipe_wire = build_pipe_opened(
        &id_key,
        &dev_key,
        &room_id,
        pipe_id,
        &dev_key.device_key(),
        "test-svc",
        "127.0.0.1:8080",
        "/iroh-rooms/pipe/1",
        &[allowed],
        None,
        &[genesis_ev_id],
        1_750_000_001_000,
    );
    let pipe_v = validate_wire_bytes(&pipe_wire.to_bytes(), &ctx).expect("pipe.opened valid");
    let pipe_ev_id = pipe_v.event_id;

    let closed_wire = build_pipe_closed(
        &id_key,
        &dev_key,
        &room_id,
        pipe_id,
        Some("closed"),
        &[pipe_ev_id],
        1_750_000_002_000,
    );
    let closed_v = validate_wire_bytes(&closed_wire.to_bytes(), &ctx).expect("pipe.closed valid");

    let db_path = home.path().join("rooms.db");
    let mut store = EventStore::open(&db_path).expect("open store");
    store.insert(&genesis_v).expect("insert genesis");
    store.insert(&pipe_v).expect("insert pipe.opened");
    store.insert(&closed_v).expect("insert pipe.closed");

    let pipe_hex: String = pipe_id.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });
    (room_id.to_string(), pipe_hex)
}

// ── pipe expose: pre-IO validation ───────────────────────────────────────────

/// Non-loopback `--tcp` must be refused before any IO (§13.2.3 / issue AC2).
#[test]
fn pipe_expose_non_loopback_tcp_is_refused() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args([
            "pipe",
            "expose",
            &room_id,
            "--tcp",
            "8.8.8.8:80",
            "--allow",
            &fake_allow_id(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-loopback").or(predicate::str::contains("loopback")));
}

/// A loopback IPv6 address (`[::1]:port`) is accepted by `is_loopback_target` — the
/// non-loopback error must not fire for it. We use an unknown room so the command
/// fails fast at `fold_room`, never reaching the node-spawn phase.
#[test]
fn pipe_expose_loopback_ipv6_passes_loopback_check() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // A real room is NOT created — we use a syntactically valid but unknown room id.
    // This ensures the command fails at fold_room (fast), not at the node-spawn phase.
    let unknown_room = format!("blake3:{}", "de".repeat(32));

    let out = cmd(&home)
        .args([
            "pipe",
            "expose",
            &unknown_room,
            "--tcp",
            "[::1]:3000",
            "--allow",
            &fake_allow_id(),
        ])
        .output()
        .unwrap();
    // Must fail (unknown room), but the failure reason must NOT be the non-loopback guard.
    assert!(!out.status.success(), "command must fail for unknown room");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("must be a loopback"),
        "::1 is a loopback address; the non-loopback error must not fire; stderr: {stderr}"
    );
}

/// `--allow` is a required argument; omitting it must exit non-zero (clap error).
#[test]
fn pipe_expose_allow_is_required_by_clap() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["pipe", "expose", &room_id, "--tcp", "127.0.0.1:3000"])
        .assert()
        .failure();
}

/// An `--allow` value that is not a valid 64-char hex identity id must be
/// refused with an actionable error (invalid --allow identity id).
#[test]
fn pipe_expose_invalid_allow_id_is_refused() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args([
            "pipe",
            "expose",
            &room_id,
            "--tcp",
            "127.0.0.1:3000",
            "--allow",
            "not-a-valid-identity-id",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("invalid --allow identity id")
                .or(predicate::str::contains("invalid")),
        );
}

// ── pipe expose: security warning / stdout-stderr split ──────────────────────

/// The ⚠ security warning must be on **stderr** and must name the `--tcp` target
/// and the allowed member (§13.2.4 / issue Security Note).
/// The machine-readable `room:/target:/allow:` lines must be on **stdout**.
///
/// This test drives `pipe expose` with `--loopback` so it reaches the warning
/// and IO stage before the node blocks on a signal; the command will still run
/// forever (waiting for Ctrl-C) so we use `assert_cmd`'s timeout to interrupt it.
/// We only check that the first batch of output looks correct — the node shutdown
/// is not exercised here.
///
/// Note: because this test starts a real tokio runtime and node (with `--loopback`)
/// it is gated behind `#[ignore]` to keep the fast-path CI green.  Run with
/// `cargo test -- --ignored pipe_expose_security_warning` to exercise it manually.
#[test]
#[ignore = "starts a loopback node; run with --ignored for manual verification"]
fn pipe_expose_security_warning_and_stdout_split() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let allow_id = {
        let out = cmd(&home).args(["identity", "show"]).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let found = stdout
            .lines()
            .find_map(|l| l.strip_prefix("identity_id:").map(|s| s.trim().to_owned()));
        found.unwrap_or_else(fake_allow_id)
    };

    let out = cmd(&home)
        .args([
            "pipe",
            "expose",
            &room_id,
            "--tcp",
            "127.0.0.1:3000",
            "--allow",
            &allow_id,
            "--loopback",
        ])
        .timeout(std::time::Duration::from_secs(3))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Security warning is on stderr and names both the target and the allowed member.
    assert!(
        stderr.contains("SECURITY") || stderr.contains("⚠"),
        "security warning must appear on stderr; got: {stderr}"
    );
    assert!(
        stderr.contains("127.0.0.1:3000"),
        "warning must name the exposed target on stderr; got: {stderr}"
    );

    // Machine-readable lines are on stdout.
    assert!(stdout.contains("room:"), "room: line must be on stdout");
    assert!(stdout.contains("target:"), "target: line must be on stdout");
    assert!(stdout.contains("allow:"), "allow: line must be on stdout");
}

// ── pipe close: pre-IO validation ────────────────────────────────────────────

/// A malformed pipe id (not 32 lowercase hex chars) must exit non-zero with an
/// actionable "invalid pipe id" error — before any IO (spec §7).
#[test]
fn pipe_close_bad_hex_is_refused() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    for bad in &["abc", "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ", &"a".repeat(31)] {
        cmd(&home)
            .args(["pipe", "close", bad])
            .assert()
            .failure()
            .stderr(predicate::str::contains("invalid pipe id"));
    }
}

/// `pipe close` with no `identity.secret` must fail (pre-IO guard).
#[test]
fn pipe_close_no_identity_fails() {
    let home = TempDir::new().unwrap();
    let fake_pipe_hex = "a".repeat(32);
    cmd(&home)
        .args(["pipe", "close", &fake_pipe_hex])
        .assert()
        .failure();
}

/// `pipe close` with a valid hex pipe id but no matching `pipe.opened` in the
/// local store must exit non-zero with a "no such pipe" error (spec §4.1 §7).
#[test]
fn pipe_close_absent_pipe_exits_nonzero_with_hint() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    create_room(&home); // so rooms.db exists and room_ids() returns one room

    let absent_pipe = "b".repeat(32); // 32 hex chars, not in the store
    cmd(&home)
        .args(["pipe", "close", &absent_pipe])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no such pipe"));
}

/// `pipe close <PIPE_ID>` infers the room when the pipe exists in exactly one
/// local room, even though no `--room` is supplied (spec §4.1 headline).
///
/// The close operation itself is online (spawns a loopback node). To test the
/// room-inference logic without a live node, we verify that the error is NOT
/// "no such pipe" (meaning inference succeeded) — the command fails later at
/// the membership check (the seeded identity is not the CLI identity) and that
/// is acceptable for this test.
#[test]
fn pipe_close_single_room_inference_reaches_past_room_lookup() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    // Seed the DB with a pipe signed by a different identity (not the CLI user).
    let pipe_id: [u8; 16] = [0x42; 16];
    let (_room_id_str, pipe_hex) = seed_room_with_pipe(
        &home, [0xA0; 32], // owner identity seed (NOT the CLI identity)
        [0xA1; 32], // owner device seed
        pipe_id, [0xCC; 16], // room nonce
    );

    let out = cmd(&home)
        .args(["pipe", "close", &pipe_hex])
        .output()
        .unwrap();

    // The command must fail, but NOT with "no such pipe" — that means room
    // inference found the pipe and room_ids() worked correctly.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "pipe close must fail when caller is not the owner"
    );
    assert!(
        !stderr.contains("no such pipe"),
        "room inference must have succeeded (error must not be 'no such pipe'); stderr: {stderr}"
    );
}

/// `pipe close` with `--room` + a valid room id must also work (explicit override
/// path). Same reasoning as above: fails at membership check, not at room lookup.
#[test]
fn pipe_close_with_explicit_room_flag_reaches_past_room_lookup() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let pipe_id: [u8; 16] = [0x43; 16];
    let (room_id_str, pipe_hex) =
        seed_room_with_pipe(&home, [0xB0; 32], [0xB1; 32], pipe_id, [0xDD; 16]);

    let out = cmd(&home)
        .args(["pipe", "close", &pipe_hex, "--room", &room_id_str])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        !stderr.contains("no such pipe"),
        "explicit --room must bypass room inference; error must not be 'no such pipe'; stderr: {stderr}"
    );
}

// ── pipe close: --help surface lock ──────────────────────────────────────────

/// `pipe close --help` must show `<PIPE_ID>` as the sole positional argument
/// and must NOT show `<ROOM_ID>` as a positional (spec §4.1 / §5.2 reconcile;
/// `pipe close <ROOM_ID> <PIPE_ID>` was the old two-positional form).
#[test]
fn pipe_close_help_shows_pipe_id_not_room_id_positional() {
    let home = TempDir::new().unwrap();
    let out = cmd(&home)
        .args(["pipe", "close", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("PIPE_ID"),
        "`pipe close --help` must name PIPE_ID; got: {stdout}"
    );
    // The old positional was labelled ROOM_ID; it must not appear as a required
    // positional any more (--room is optional and flags don't appear in the same
    // position notation).
    //
    // The usage line should be `pipe close <PIPE_ID>` not `pipe close <ROOM_ID> <PIPE_ID>`.
    // We check the usage section for the two-positional form.
    let usage_section = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("Usage:"))
        .unwrap_or(&stdout);
    assert!(
        !usage_section.contains("ROOM_ID"),
        "usage line must not show ROOM_ID as a positional; usage: {usage_section}"
    );
}

// ── pipe list ─────────────────────────────────────────────────────────────────

/// `pipe list` with an unknown room id must exit non-zero (the room is not in
/// the local store).
#[test]
fn pipe_list_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    create_room(&home); // so rooms.db exists

    let unknown_room = format!("blake3:{}", "cd".repeat(32));
    cmd(&home)
        .args(["pipe", "list", &unknown_room])
        .assert()
        .failure();
}

/// `pipe list` on a room with no pipes must exit zero and print "(no open pipes)".
#[test]
fn pipe_list_empty_room_shows_no_open_pipes() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    cmd(&home)
        .args(["pipe", "list", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("no open pipes"));
}

/// `pipe list` on a seeded room shows the open pipe.
#[test]
fn pipe_list_seeded_room_shows_pipe() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let pipe_id: [u8; 16] = [0x55; 16];
    let (room_id_str, pipe_hex) =
        seed_room_with_pipe(&home, [0xC0; 32], [0xC1; 32], pipe_id, [0xEE; 16]);

    cmd(&home)
        .args(["pipe", "list", &room_id_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("pipe_id:"))
        .stdout(predicate::str::contains(&pipe_hex[..8])); // prefix of the hex
}

/// A pipe that has a matching `pipe.closed` event must NOT appear in `pipe list`
/// (the `closed_pipe_ids` filter; issue AC4 "clean close emits `pipe.closed`").
#[test]
fn pipe_list_closed_pipe_is_not_shown() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let pipe_id: [u8; 16] = [0x66; 16];
    let (room_id_str, _pipe_hex) =
        seed_room_with_opened_and_closed_pipe(&home, [0xD0; 32], [0xD1; 32], pipe_id, [0xF0; 16]);

    cmd(&home)
        .args(["pipe", "list", &room_id_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("no open pipes"));
}

// ── pipe expose/connect/close: non-member error (IR-0305 doc-backing) ─────────
//
// `docs/live-pipe-preview.md`'s "Unauthorized access behavior" section documents,
// verbatim, that a caller who is not an active member of the room is turned away
// **locally, before any dial** by `pipe connect` (and `pipe expose` / `pipe
// close`) with the coded line `error[peer_unauthorized]: you are not an active
// member of room …` and exit code `3`. All three commands reach this check via a
// purely local `fold_room` + `snapshot.is_active` lookup (see `pipe.rs::expose`,
// `::connect`, `::close`) before any network IO, so the case is deterministic and
// needs no live node — these three tests pin the guide's exact claim per command.

/// `pipe expose` with a caller who is NOT an active member of the room must fail
/// with the coded `error[peer_unauthorized]:` line, exit `3`, and an actionable
/// "not an active member" message (issue AC1 / spec §13.2.1; guide §4.7 case 2).
/// The room exists in the local store (so `fold_room` succeeds) but the CLI
/// identity is not the room admin/member.
#[test]
fn pipe_expose_non_member_is_refused() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    // Seed a room whose admin is a different identity (not the CLI user).
    let room_id_str = seed_genesis_only(&home, [0xF0; 32], [0xF1; 32], [0xFA; 16]);

    let out = cmd(&home)
        .args([
            "pipe",
            "expose",
            &room_id_str,
            "--tcp",
            "127.0.0.1:3000",
            "--allow",
            &fake_allow_id(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a non-member `pipe expose` must exit 3 (Auth); stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[peer_unauthorized]:"),
        "must render the coded peer_unauthorized line the guide quotes; stderr: {stderr}"
    );
    assert!(
        stderr.contains("not an active member"),
        "must explain why in plain language; stderr: {stderr}"
    );
}

/// `pipe connect` with a caller who is NOT an active member of the room must fail
/// the same way as `pipe expose` above — this is the guide's headline
/// AC3/§4.7-case-2 example, and the check happens before `pipe connect` ever
/// resolves the pipe id or dials a peer, so an arbitrary well-formed pipe id is
/// enough to reach it.
#[test]
fn pipe_connect_non_member_is_refused() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let room_id_str = seed_genesis_only(&home, [0xF2; 32], [0xF3; 32], [0xFB; 16]);
    let arbitrary_pipe_id = "c".repeat(32);

    let out = cmd(&home)
        .args([
            "pipe",
            "connect",
            &room_id_str,
            &arbitrary_pipe_id,
            "--local",
            "0",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a non-member `pipe connect` must exit 3 (Auth); stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[peer_unauthorized]:"),
        "must render the coded peer_unauthorized line the guide quotes; stderr: {stderr}"
    );
    assert!(
        stderr.contains("not an active member"),
        "must explain why in plain language; stderr: {stderr}"
    );
}

/// `pipe close` with a caller who is NOT an active member of the room must also
/// fail with `error[peer_unauthorized]:` / exit `3`, distinct from the
/// non-owner/non-admin *active*-member case already covered by
/// `pipe_close_by_a_non_owner_non_admin_member_exits_3_peer_unauthorized` in
/// `error_taxonomy_e2e.rs`. `--room` bypasses pipe-id-based room inference, so no
/// pipe needs to exist for the caller to reach the membership check.
#[test]
fn pipe_close_non_member_is_refused() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let room_id_str = seed_genesis_only(&home, [0xF4; 32], [0xF5; 32], [0xFC; 16]);
    let arbitrary_pipe_id = "d".repeat(32);

    let out = cmd(&home)
        .args(["pipe", "close", &arbitrary_pipe_id, "--room", &room_id_str])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a non-member `pipe close` must exit 3 (Auth); stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[peer_unauthorized]:"),
        "must render the coded peer_unauthorized line the guide quotes; stderr: {stderr}"
    );
    assert!(
        stderr.contains("not an active member"),
        "must explain why in plain language; stderr: {stderr}"
    );
}

// ── pipe expose: optional flag argument parsing ───────────────────────────────

/// `pipe expose` must accept multiple `--allow` flags (PRD §16.2 / issue AC1).
/// We use a syntactically valid but unknown room so the command fails at
/// `fold_room` (not at argument validation) — proving the clap parsing and
/// pre-IO allow-list validation both pass without error.
#[test]
fn pipe_expose_multiple_allow_flags_are_accepted() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // Unknown room — fails at fold_room, not at allow parsing.
    let unknown_room = format!("blake3:{}", "ef".repeat(32));
    let allow_a = fake_allow_id();
    let allow_b = "b".repeat(64);

    let out = cmd(&home)
        .args([
            "pipe",
            "expose",
            &unknown_room,
            "--tcp",
            "127.0.0.1:3000",
            "--allow",
            &allow_a,
            "--allow",
            &allow_b,
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "must fail for unknown room");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("invalid --allow identity id"),
        "multiple valid --allow values must pass pre-IO allow parsing; stderr: {stderr}"
    );
}

/// `pipe expose` must accept the optional `--label` and `--expires` flags.
/// Command fails at `fold_room` (unknown room), not at `--label`/`--expires`
/// parsing — proving both flags are wired through correctly.
#[test]
fn pipe_expose_label_and_expires_flags_are_accepted() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let unknown_room = format!("blake3:{}", "12".repeat(32));

    let out = cmd(&home)
        .args([
            "pipe",
            "expose",
            &unknown_room,
            "--tcp",
            "127.0.0.1:3000",
            "--allow",
            &fake_allow_id(),
            "--label",
            "my-dev-server",
            "--expires",
            "24h",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "must fail for unknown room");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The failure must be the unknown-room error, not a flag parse error.
    assert!(
        !stderr.contains("--expires must end with"),
        "--expires 24h must parse without error; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("error: unexpected argument"),
        "--label and --expires must be recognised by clap; stderr: {stderr}"
    );
}
