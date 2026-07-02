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
//!
//! IR-0303 (issue #38) extends this suite with the additive `next:` render line and
//! the coded-argument uniformity fix — the human half of the same surface:
//!   AC1 next: line           — `coded_failure_renders_error_then_a_single_next_action_line`,
//!                              `representative_coded_failures_each_render_a_next_action`,
//!                              `room_not_found_renders_the_one_canonical_next_action`,
//!                              `file_share_missing_path_renders_a_next_action`,
//!                              `file_share_too_large_renders_a_next_action`
//!   AC2 machine surface      — `code_without_a_next_action_emits_no_next_line`,
//!                              `uncoded_failure_emits_no_next_line`,
//!                              `file_fetch_bad_timeout_exits_2_with_coded_line`
//!   §5.3 verbose guard       — `members_verbose_requires_status`,
//!                              `room_tail_accepts_verbose_and_parses_room_id_first`,
//!                              `room_tail_verbose_conflicts_with_offline`

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

/// Corrupt a ticket so it **deterministically** fails its trailing BLAKE3 checksum
/// (`ticket_bad_checksum`) while staying a fully base32-decodable token — a realistic
/// single-character copy-paste garble.
///
/// We flip a character in the **middle** of the base32 body (after the `roomtkt1`
/// prefix), which always maps to a payload byte. A naive *last*-char flip is flaky
/// (~1/32): the final char carries the RFC-4648 canonical zero-padding bits, so when
/// the minted ticket happens to end in `a`, flipping to `b` sets a padding bit and a
/// strict decoder reports `ticket_bad_base32`, not a checksum failure. The *first*
/// char decodes into the version byte, which is checked before the checksum. A middle
/// char dodges both: it alters one payload byte with the version byte and length
/// intact, so the appended checksum no longer matches.
fn corrupt_last_char(ticket: &str) -> String {
    let prefix_len = "roomtkt1".len();
    let mut chars: Vec<char> = ticket.chars().collect();
    let mid = prefix_len + (chars.len() - prefix_len) / 2;
    chars[mid] = if chars[mid] == 'a' { 'b' } else { 'a' };
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

// ── IR-0303 AC1: the additive `next:` actionable-step render line (spec §5.1) ──
//
// Issue #38 adds a second stderr line, `next: <action>`, under every coded failure
// whose `ErrorCode::next_action()` is `Some` — the human "what do I do now" half of
// the taxonomy surface. These tests pin the *render contract* (main.rs) that the
// `src/error.rs` unit tests cannot reach: that the line is actually emitted, is
// singular (no double next-step after the §5.1 message migration), and never
// perturbs the machine surface the taxonomy issue promised (the first stderr line
// still starts `error[`, the exit code is unchanged). All cases are pre-IO /
// offline — no node is brought up.

/// The number of `next:` lines on `stderr` (each a distinct actionable step). The
/// render contract emits at most one; the migration rule (spec §5.1) forbids two.
fn count_next_lines(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.starts_with("next:")).count()
}

/// Run a command to completion and return `(exit_code, stderr)` for line-shape
/// assertions the `assert_cmd` predicate API cannot express directly.
fn run_stderr(home: &TempDir, args: &[&str]) -> (Option<i32>, String) {
    let out = cmd(home).args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A coded failure whose code is user-actionable renders `error[<code>]:` as the
/// **first** stderr line and, immediately after it, exactly one `next: <action>`
/// line — the AC1 surface. The machine line is unchanged (greppable `^error\[`),
/// and the exit code is still the code's category (Usage / 2 here).
#[test]
fn coded_failure_renders_error_then_a_single_next_action_line() {
    let home = TempDir::new().unwrap();
    let (code, stderr) = run_stderr(&home, &["identity", "show"]);

    assert_eq!(code, Some(2), "identity_not_found is Usage / exit 2");
    let mut lines = stderr.lines();
    let first = lines.next().unwrap_or_default();
    assert!(
        first.starts_with("error[identity_not_found]:"),
        "the machine line must be first and greppable; got first line: {first:?}"
    );
    // The `next:` line follows immediately (this message is single-line) …
    let second = lines.next().unwrap_or_default();
    assert!(
        second.starts_with("next:"),
        "a `next:` line must immediately follow the coded error; got: {second:?}"
    );
    // … names the concrete action (migrated out of the trimmed message) …
    assert!(
        second.contains("identity create"),
        "the next: line must name the actionable step; got: {second:?}"
    );
    // … and is the *only* one (no double next-step; spec §5.1 migration rule).
    assert_eq!(
        count_next_lines(&stderr),
        1,
        "exactly one next: line must be emitted; got:\n{stderr}"
    );
}

/// AC1 breadth: a representative coded failure from each of several categories —
/// Usage (`invalid_room_id`, `room_not_found`) and Ticket (`ticket_bad_prefix`) —
/// each renders the `error[<code>]:` line first and a single following `next:`
/// line, with the exit code unchanged. Table-driven so a regression in any one
/// code's rendering is caught in one place.
#[test]
fn representative_coded_failures_each_render_a_next_action() {
    let unknown_room = format!("blake3:{}", "0".repeat(64));
    let cases: &[(&[&str], &str, i32)] = &[
        (&["room", "members", "notaroomid"], "invalid_room_id", 2),
        (&["room", "members", &unknown_room], "room_not_found", 2),
        (&["room", "join", "nothello"], "ticket_bad_prefix", 5),
    ];
    for (args, expected_code, expected_exit) in cases {
        let home = TempDir::new().unwrap();
        let (code, stderr) = run_stderr(&home, args);
        assert_eq!(
            code,
            Some(*expected_exit),
            "`{args:?}` must exit {expected_exit} for {expected_code}"
        );
        let first = stderr.lines().next().unwrap_or_default();
        assert!(
            first.starts_with(&format!("error[{expected_code}]:")),
            "`{args:?}` must render error[{expected_code}] first; got: {first:?}"
        );
        assert_eq!(
            count_next_lines(&stderr),
            1,
            "`{args:?}` must render exactly one next: line; got:\n{stderr}"
        );
    }
}

/// The §5.1 "`room_not_found` fix": the `room members` offline path (which bottoms
/// out at the `room.rs` bare-message site) now renders the single, canonical
/// `RoomNotFound` next action — the same string every other `room_not_found` site
/// shares. Pins the *rendered* consistency the `src/error.rs` unit test asserts at
/// the API level, closing the loop from emitter to terminal.
#[test]
fn room_not_found_renders_the_one_canonical_next_action() {
    let home = TempDir::new().unwrap();
    let unknown = format!("blake3:{}", "0".repeat(64));
    let (code, stderr) = run_stderr(&home, &["room", "members", &unknown]);
    assert_eq!(code, Some(2), "room_not_found is Usage / exit 2");
    let next = stderr
        .lines()
        .find(|l| l.starts_with("next:"))
        .expect("room_not_found must carry a next: line");
    // Distinctive phrase from the single `RoomNotFound.next_action()` template.
    assert!(
        next.contains("join an invite ticket first"),
        "the canonical room_not_found next action must be rendered; got: {next:?}"
    );
}

/// AC1 breadth (spec §8 #3): a `file share` of a **missing** path renders the coded
/// `error[no_such_file]:` machine line **first** and, additively, exactly one
/// `next:` line naming the concrete step — the file-share half of the §2.2 gap
/// IR-0303 filled (before this issue `no_such_file` was a bare message with no fix).
/// Needs an identity + room so the membership gate passes and `classify_path` is
/// actually reached; the failure is offline (no node is brought up).
#[test]
fn file_share_missing_path_renders_a_next_action() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let missing = home.path().join("does-not-exist.txt");
    let (code, stderr) = run_stderr(
        &home,
        &["file", "share", &room_id, missing.to_str().unwrap()],
    );

    assert_eq!(code, Some(2), "no_such_file is Usage / exit 2");
    assert!(
        stderr
            .lines()
            .next()
            .unwrap_or_default()
            .starts_with("error[no_such_file]:"),
        "the coded machine line must be first and greppable; got:\n{stderr}"
    );
    let next = stderr
        .lines()
        .find(|l| l.starts_with("next:"))
        .expect("no_such_file must now carry a next: line (IR-0303 AC1)");
    assert!(
        next.contains("check the path"),
        "the next: line must name the actionable step; got: {next:?}"
    );
    assert_eq!(
        count_next_lines(&stderr),
        1,
        "exactly one next: line must be emitted; got:\n{stderr}"
    );
}

/// AC1 breadth (spec §8 #3): a `file share` over the (test-lowered) size cap renders
/// `error[file_too_large]:` first and exactly one following `next:` line ("split or
/// compress"). The `IROH_ROOMS_MAX_SHARE_BYTES` seam hits the boundary without a huge
/// fixture. Classified offline before any store/blob write, so no node is spun up.
#[test]
fn file_share_too_large_renders_a_next_action() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let big = home.path().join("big.bin");
    std::fs::write(&big, vec![0u8; 64]).unwrap();

    let out = cmd(&home)
        .env("IROH_ROOMS_MAX_SHARE_BYTES", "1")
        .args(["file", "share", &room_id])
        .arg(&big)
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(2),
        "file_too_large is Usage / exit 2"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr
            .lines()
            .next()
            .unwrap_or_default()
            .starts_with("error[file_too_large]:"),
        "the coded machine line must be first; got:\n{stderr}"
    );
    let next = stderr
        .lines()
        .find(|l| l.starts_with("next:"))
        .expect("file_too_large must carry a next: line (IR-0303 AC1)");
    assert!(
        next.contains("split or compress"),
        "the next: line must name the actionable step; got: {next:?}"
    );
    assert_eq!(
        count_next_lines(&stderr),
        1,
        "exactly one next: line must be emitted; got:\n{stderr}"
    );
}

/// AC2 (machine surface intact): a coded failure whose `next_action()` is `None`
/// (`invalid_argument`) still renders the `error[<code>]:` line and exits with its
/// category, but emits **no** `next:` line — the `if let Some(next)` suppression in
/// `main.rs`. Guards against a regression that would print an empty/`next:` line for
/// context-only codes.
#[test]
fn code_without_a_next_action_emits_no_next_line() {
    let home = TempDir::new().unwrap();
    let (code, stderr) = run_stderr(
        &home,
        &["room", "join", "anytoken", "--timeout", "notaduration"],
    );
    assert_eq!(code, Some(2), "invalid_argument is Usage / exit 2");
    assert!(
        stderr.starts_with("error[invalid_argument]:"),
        "the coded line must still render; got: {stderr}"
    );
    assert_eq!(
        count_next_lines(&stderr),
        0,
        "a None-next_action code must not emit a next: line; got:\n{stderr}"
    );
}

/// AC2 negative: the uncoded long-tail fallback (`error:` with no `[code]`) never
/// carries a `next:` line — the actionable step is a property of an `ErrorCode`, and
/// an uncoded failure has none. Pins that the `next:` machinery is reached only on
/// the coded arm of `main.rs`.
#[test]
fn uncoded_failure_emits_no_next_line() {
    let home = TempDir::new().unwrap();
    let (code, stderr) = run_stderr(&home, &["room", "create", ""]);
    assert_eq!(code, Some(1), "an uncoded failure exits 1");
    assert!(
        stderr.starts_with("error:") && !stderr.contains("error["),
        "the uncoded fallback must render `error:` with no [code]; got: {stderr}"
    );
    assert_eq!(
        count_next_lines(&stderr),
        0,
        "an uncoded failure must not emit a next: line; got:\n{stderr}"
    );
}

// ── IR-0303 AC2 uniformity: the `file fetch --timeout` coded-argument fix (§2.2) ─

/// A malformed `--timeout` on **`file fetch`** now emits the coded
/// `error[invalid_argument]:` line and exits `2`, matching its four sibling timeout
/// sites (`room join`/`send`/`members`/`agent status`). Before IR-0303 this one site
/// (`cli.rs:538`) parsed with a bare `?`, so a bad value took the uncoded `error:` /
/// exit-1 path — breaking the "every bad argument is `invalid_argument` / exit 2"
/// contract scripts rely on. The timeout is parsed pre-IO (after `parse_room_id`),
/// so this needs no identity, room, or reachable provider. Regression guard.
#[test]
fn file_fetch_bad_timeout_exits_2_with_coded_line() {
    let home = TempDir::new().unwrap();
    let room = format!("blake3:{}", "0".repeat(64));
    cmd(&home)
        .args([
            "file",
            "fetch",
            &room,
            "file_deadbeef",
            "--timeout",
            "notaduration",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[invalid_argument]:"));
}

// ── IR-0303 §5.3: `--verbose` is guarded to `--status` on `room members` ───────

/// `room members --verbose` without `--status` is rejected (clap `requires`), so the
/// opt-in `diag:` block is only ever produced by the node-bearing status path (spec
/// §5.3 "guard: only meaningful with --status"; OQ-6). A bad combination is a usage
/// error (exit 2) naming the missing `--status`, not a silently-ignored flag. The
/// live `diag:` rendering itself is a two-peer / loopback concern (e2e phase).
#[test]
fn members_verbose_requires_status() {
    let home = TempDir::new().unwrap();
    let room = format!("blake3:{}", "0".repeat(64));
    let (code, stderr) = run_stderr(&home, &["room", "members", &room, "--verbose"]);
    assert_eq!(
        code,
        Some(2),
        "a bad flag combination is a clap usage error / exit 2"
    );
    assert!(
        stderr.contains("--status"),
        "the error must name the required --status flag; got: {stderr}"
    );
}

/// §5.3: `--verbose` is wired onto **`room tail`** too (spec adds it to both status
/// commands), and `parse_room_id` runs before any node is brought up. So
/// `room tail <malformed-id> --verbose` fails fast with the coded
/// `error[invalid_room_id]:` line (exit 2) — which proves clap *accepted* `--verbose`
/// on tail (a missing flag would instead be a clap "unexpected argument" usage error)
/// *and* that the id is parsed pre-node, without ever spinning up the live tail loop.
/// The live `diag:` rendering itself stays a loopback / two-peer (e2e) concern.
#[test]
fn room_tail_accepts_verbose_and_parses_room_id_first() {
    let home = TempDir::new().unwrap();
    let (code, stderr) = run_stderr(&home, &["room", "tail", "notaroomid", "--verbose"]);

    assert_eq!(code, Some(2), "a malformed room id is Usage / exit 2");
    assert!(
        stderr.contains("error[invalid_room_id]:"),
        "`--verbose` must be a recognized tail flag and the id parsed pre-node; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("unexpected argument"),
        "clap must accept `--verbose` on tail, not reject it as unknown; got:\n{stderr}"
    );
    // invalid_room_id is user-actionable, so exactly one next: line accompanies it.
    assert_eq!(
        count_next_lines(&stderr),
        1,
        "invalid_room_id carries one next: line; got:\n{stderr}"
    );
}

/// §5.3 guard: on `room tail`, `--verbose` is a *live-view* flag clap declares
/// `conflicts_with = "offline"` (the diagnostics read a running node's transport; an
/// `--offline` local read has none to classify). `room tail <id> --verbose --offline`
/// is therefore a clap usage error (exit 2) naming the conflicting flags — pinning the
/// declared guard so a future edit can't silently allow the meaningless combination.
/// The conflict is caught at parse time, before dispatch, so no node is brought up.
#[test]
fn room_tail_verbose_conflicts_with_offline() {
    let home = TempDir::new().unwrap();
    let room = format!("blake3:{}", "0".repeat(64));
    let (code, stderr) = run_stderr(&home, &["room", "tail", &room, "--verbose", "--offline"]);

    assert_eq!(
        code,
        Some(2),
        "a conflicting flag combination is a clap usage error / exit 2"
    );
    assert!(
        stderr.contains("--verbose") && stderr.contains("--offline"),
        "the error must name the conflicting --verbose/--offline flags; got:\n{stderr}"
    );
}
