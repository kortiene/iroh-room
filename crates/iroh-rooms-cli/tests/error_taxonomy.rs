//! CLI error-taxonomy integration tests (spec IR-0110 / issue #25, §8).
//!
//! Every other CLI integration suite asserts only `.failure()` (non-zero) plus a
//! human-message substring. This suite pins the two contracts the taxonomy issue
//! actually introduces and that nothing else checks:
//!
//!   1. the machine-parseable render line `error[<code>]: …` on stderr, and
//!   2. the stable **category exit code** (§5.3: 2=Usage, 3=Auth, 5=Ticket) so a
//!      script can branch on `$?`.
//!
//! Scope is deterministic and **network-free**: only the pre-IO and offline
//! failure paths are exercised here (input/environment, ticket decode, wrong
//! identity). The receive-path advisories and the connectivity codes
//! (`bad_signature`/`not_a_member` warnings, `no_admin_reachable`, `peer_offline`)
//! require a live session and belong to the e2e phase; they are covered by their
//! pinned `.code()`/`exit_code()` unit tests in `src/error.rs` and `src/ticket.rs`.
//!
//! Coverage map:
//!   uncoded fallback (§5.2)  — `uncoded_failure_renders_plain_error_and_exits_1`
//!   Usage / exit 2           — `invalid_room_id_*`, `identity_not_found_*`,
//!                              `room_not_found_*`, `invalid_argument_bad_timeout_*`,
//!                              `no_such_file_*`, `file_too_large_*`
//!   Ticket / exit 5 (AC3)    — `ticket_bad_prefix_*`, `ticket_bad_base32_*`,
//!                              `ticket_truncated_*`, `ticket_bad_checksum_*`
//!   Auth / exit 3 (AC3)      — `wrong_identity_*`
//!   AC3 secret hygiene       — `corrupted_ticket_never_echoes_token_or_secret`

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn cmd_at(path: &Path) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME").arg("--data-dir").arg(path);
    c
}

fn cmd(home: &TempDir) -> Command {
    cmd_at(home.path())
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
fn extract_ticket(stdout: &str) -> Option<String> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("ticket:") {
            return lines.next().map(|l| l.trim().to_owned());
        }
    }
    None
}

/// A fixed 64-hex identity key that no deterministic seed in the suite produces —
/// used as the invite's bound `--invitee` so a freshly-created joiner never matches.
const INVITEE_HEX: &str = "0404040404040404040404040404040404040404040404040404040404040404";

/// Stand up an admin home (identity + room + invite for `invitee_hex`) and return
/// the minted `roomtkt1…` ticket string.
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
    extract_ticket(&stdout).expect("ticket must appear in `room invite` output")
}

/// Flip the final base32 character of a ticket, breaking its trailing checksum
/// while keeping the length/version byte intact (a realistic copy-paste garble).
fn corrupt_last_char(ticket: &str) -> String {
    let mut chars: Vec<char> = ticket.chars().collect();
    let last = chars.len() - 1;
    chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
    chars.into_iter().collect()
}

// ── uncoded fallback (spec §5.2) ──────────────────────────────────────────────

/// A failure the taxonomy has not adopted (here: the plain `bail!` in
/// `identity::validate_name`) must still render the generic `error: <message>`
/// line (no `[code]`) and exit `1` — the graceful long-tail contract AC4 relies on.
#[test]
fn uncoded_failure_renders_plain_error_and_exits_1() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "create", ""])
        .assert()
        .code(1)
        .stderr(predicate::str::starts_with("error:"))
        .stderr(predicate::str::contains("error[").not());
}

// ── Usage / exit 2 ────────────────────────────────────────────────────────────

/// A malformed room id fails at `parse_room_id` (pre-IO) with the coded line and
/// the Usage exit code.
#[test]
fn invalid_room_id_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "members", "notaroomid"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[invalid_room_id]:"));
}

/// No local identity: `identity show` on an empty home renders `identity_not_found`
/// (Usage / exit 2), keeping the actionable "run `identity create`" hint.
#[test]
fn identity_not_found_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "show"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[identity_not_found]:"))
        .stderr(predicate::str::contains("identity create"));
}

/// A well-formed but unknown room id (offline read) fails closed with
/// `room_not_found` (Usage / exit 2), distinct from a malformed id.
#[test]
fn room_not_found_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    let unknown = format!("blake3:{}", "0".repeat(64));
    cmd(&home)
        .args(["room", "members", &unknown])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[room_not_found]:"));
}

/// A bad `--timeout` value is parsed (and coded `invalid_argument`) before the
/// ticket is even decoded, so this needs neither an identity nor a valid ticket.
/// The coded `error[invalid_argument]:` line distinguishes it from clap's own
/// exit-2 usage error (which prints a usage block, not this prefix).
#[test]
fn invalid_argument_bad_timeout_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "anytoken", "--timeout", "notaduration"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[invalid_argument]:"));
}

/// `file share` of a missing path fails at `classify_path` (offline) with
/// `no_such_file` (Usage / exit 2). The caller is the room admin, so the
/// membership gate passes and the path classifier is actually reached.
#[test]
fn no_such_file_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let missing = home.path().join("does-not-exist.txt");
    cmd(&home)
        .args(["file", "share", &room_id])
        .arg(&missing)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[no_such_file]:"));
}

/// `file share` of a file over the (test-lowered) size cap fails with
/// `file_too_large` (Usage / exit 2). The `IROH_ROOMS_MAX_SHARE_BYTES` seam lets
/// us hit the boundary without a huge fixture (spec OQ-4 / file.rs).
#[test]
fn file_too_large_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let big = home.path().join("big.bin");
    std::fs::write(&big, vec![0u8; 64]).unwrap();
    cmd(&home)
        .env("IROH_ROOMS_MAX_SHARE_BYTES", "1")
        .args(["file", "share", &room_id])
        .arg(&big)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[file_too_large]:"));
}

/// `file share` of a directory folds under `invalid_argument` (Usage / exit 2), the
/// OQ-4 decision (spec §5.5 / file.rs: a directory is not a dedicated `not_a_file`
/// code). Classified offline before any store/blob write, like the missing-file path.
#[test]
fn directory_share_exits_2_with_invalid_argument_coded_line() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    // The home dir itself is a directory; sharing it must hit the is_dir arm.
    let dir = home.path().to_path_buf();
    cmd(&home)
        .args(["file", "share", &room_id])
        .arg(&dir)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[invalid_argument]:"));
}

/// `file share` of a `chmod 000` file fails at `classify_path`'s open-probe with
/// `permission_denied` (Usage / exit 2). Unix-only, and skipped when the test runs as
/// root (where mode `000` is still readable, so the probe would spuriously succeed).
#[cfg(unix)]
#[test]
fn unreadable_file_exits_2_with_permission_denied_coded_line() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let secret = home.path().join("secret.bin");
    std::fs::write(&secret, b"hidden").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

    // Root bypasses the mode bits; detect it by probing the open and skip if readable
    // so the assertion is not falsely violated in a root CI container.
    let running_as_root = std::fs::File::open(&secret).is_ok();
    if running_as_root {
        let _ = std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600));
        return;
    }

    let assert = cmd(&home)
        .args(["file", "share", &room_id])
        .arg(&secret)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[permission_denied]:"));
    // Restore perms so TempDir cleanup can remove the file regardless of the outcome.
    let _ = std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600));
    drop(assert);
}

// ── Ticket / exit 5 (AC3: distinct reason per decode failure) ─────────────────

/// A token without the `roomtkt1` prefix → `ticket_bad_prefix`, exit 5.
#[test]
fn ticket_bad_prefix_exits_5_with_coded_line() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "nothello"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("error[ticket_bad_prefix]:"));
}

/// The `roomtkt1` prefix with a body outside the RFC 4648 base32 alphabet
/// (`1`/`8`/`0`/`9`) → `ticket_bad_base32`, exit 5.
#[test]
fn ticket_bad_base32_exits_5_with_coded_line() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "roomtkt11809"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("error[ticket_bad_base32]:"));
}

/// A body too short to hold the version byte + 4-byte checksum → `ticket_truncated`,
/// exit 5.
#[test]
fn ticket_truncated_exits_5_with_coded_line() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", "roomtkt1aa"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("error[ticket_truncated]:"));
}

/// A real, well-formed ticket with one flipped character fails its checksum →
/// a `ticket_*` code, exit 5. (A single-char flip keeps the length and version
/// byte, so the trailing checksum is the mismatch — `ticket_bad_checksum`.)
#[test]
fn ticket_bad_checksum_exits_5_with_coded_line() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);
    let corrupted = corrupt_last_char(&ticket);

    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "join", &corrupted])
        .assert()
        .code(5)
        // The flip is engineered to break the checksum; assert the family prefix so
        // the test is robust even if a flip ever decodes to a different ticket_* arm.
        .stderr(predicate::str::contains("error[ticket_bad_checksum]:"));
}

// ── Auth / exit 3 (AC3: wrong identity for a ticket) ──────────────────────────

/// A valid ticket bound to `INVITEE_HEX` redeemed from a home holding a different
/// (freshly generated) identity → `wrong_identity`, exit 3. Fails pre-IO, before
/// any node is brought up.
#[test]
fn wrong_identity_exits_3_with_coded_line() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    let home_bob = TempDir::new().unwrap();
    cmd(&home_bob)
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    cmd(&home_bob)
        .args(["room", "join", &ticket])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("error[wrong_identity]:"));
}

// ── AC3: secret hygiene on the ticket error path (spec §5.6 / §8 #10) ─────────

/// The load-bearing AC3 property: a failing ticket decode must never echo the raw
/// token (whose base32 body embeds the capability secret) nor any decoded field.
/// We corrupt a real ticket carrying a known secret and assert that neither the
/// token (valid or corrupted) nor the secret's hex appears on any stream.
#[test]
fn corrupted_ticket_never_echoes_token_or_secret() {
    use iroh_rooms_core::ticket::RoomInviteTicket;

    let home_admin = TempDir::new().unwrap();
    let ticket = admin_invite_ticket(&home_admin, INVITEE_HEX);

    // Recover the capability secret carried in the (still valid) token so we can
    // assert it is never rendered. Parsing here is test-side only.
    let parsed: RoomInviteTicket = ticket.parse().expect("minted ticket must parse");
    let secret_hex = hex::encode(parsed.capability_secret);
    let corrupted = corrupt_last_char(&ticket);

    let home = TempDir::new().unwrap();
    let out = cmd(&home)
        .args(["room", "join", &corrupted])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(5),
        "a corrupted ticket must fail with the Ticket exit category"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !combined.contains(&secret_hex),
        "ticket error must not echo the capability secret hex"
    );
    assert!(
        !combined.contains(&corrupted),
        "ticket error must not echo the (corrupted) raw token"
    );
    assert!(
        !combined.contains(&ticket),
        "ticket error must not echo the original raw token"
    );
    // Sanity: the redacted reason IS surfaced (a coded ticket line), so the
    // no-leak guarantee is not vacuously satisfied by an empty/absent message.
    assert!(
        stderr.contains("error[ticket_"),
        "a redacted, coded ticket error must still be rendered; got: {stderr}"
    );
}
