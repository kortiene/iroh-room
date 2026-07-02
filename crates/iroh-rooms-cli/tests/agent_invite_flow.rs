//! IR-0207 (issue #32) — the **agent invite flow** as one traceable suite.
//!
//! IR-0207 asks to "allow a room admin to invite an agent participant explicitly,"
//! with a key-bound `role = agent` invite whose join uses **the same capability
//! verification as a human peer**. Every primitive is already landed and
//! role-agnostic — `agent invite` (IR-0206), the key-bound invite (IR-0103), and
//! `room join` + `gate_join` (IR-0104) — so this file adds **no production code**:
//! it is the flow-level conformance proof that the four Test-Plan legs hold
//! *through the agent surface*, and it closes the one leg IR-0206's `agent_e2e.rs`
//! deliberately deferred — AC3 / "bad ticket": an agent join **rejected without a
//! valid capability**.
//!
//! ## What this file owns (deterministic, network-free — the always-green tier)
//!
//! | IR-0207 leg / AC | Test | New coverage? |
//! |---|---|---|
//! | Leg 1 / AC1 — admin invites an agent by key | `agent_invite_mints_key_bound_agent_ticket` | flow re-assertion (exhaustive: `agent_cli.rs`) |
//! | Leg 3 / AC2 — non-admin cannot invite an agent | `non_admin_cannot_invite_an_agent` | flow re-assertion (backstop: `agent_cli.rs`) |
//! | Leg 4a / AC3 — corrupt agent ticket → `ticket_*` | `corrupt_agent_ticket_rejected_with_ticket_code_and_no_membership` | **YES** |
//! | Leg 4a / AC3 — truncated agent ticket → `ticket_*` | `truncated_agent_ticket_rejected_with_ticket_category` | **YES** |
//! | Leg 4a / AC3 — agent ticket, wrong identity → `wrong_identity` | `wrong_identity_agent_ticket_rejected_pre_io_names_bound_key` | **YES** |
//! | Leg 4a / AC3 — no capability secret ever echoed | `corrupt_agent_ticket_never_echoes_token_or_capability_secret` | **YES** |
//! | Leg 4a / AC3 core — corrupt agent vs human → identical code | `agent_and_human_corrupt_ticket_reject_with_identical_code` | **YES** |
//! | Leg 4a / AC3 core — wrong-identity agent vs human → identical code | `agent_and_human_wrong_identity_reject_with_identical_code` | **YES** |
//!
//! The last two tests pin the AC3 *core* — "the same capability verification as a
//! human peer" — by asserting the agent-flow rejection **code** is byte-identical
//! to the human-flow code (not merely present). Because the codec and the
//! key-binding pre-check never branch on `role`, a future refactor that
//! special-cased `agent` (spec R5) would diverge one code and fail these; they are
//! the durable guard that the agent stays gated exactly as tightly as a human.
//!
//! The new leg-4a coverage is the point of the issue: the agent — the PRD's
//! least-trusted principal (§13.3) — is gated **exactly as tightly as a human**.
//! Because `gate_join`/the ticket codec/the key-binding pre-check never branch on
//! `role`, the agent-flavored rejection codes are byte-identical to the human ones
//! (`ticket_bad_checksum` / `wrong_identity` — the same strings the member-role
//! tests assert), which *is* the "same capability verification as a human peer"
//! acceptance criterion, asserted rather than assumed. These are the two failure
//! classes that fail **before any network or store IO** (ticket decode + the
//! key-binding pre-check in `join::join`), so they are fully deterministic here.
//!
//! ## What lives in the e2e phase (not here)
//!
//! Leg 2 (happy-path agent join converges to `role: agent, status: active` on both
//! peers) and leg 4b (an *online* wrong-capability-secret / expired agent join
//! refused by a live admin's `gate_join`) both require two live loopback nodes or
//! two in-process `Node`s to rendezvous. They follow the `#[ignore]`-gated
//! `--loopback` convention (leg 2) or run always-green at the Node layer (leg 4b,
//! deterministic — no network flakiness to gate) and belong to the cross-boundary
//! e2e suites:
//! - Leg 2: `agent_e2e.rs::agent_joins_and_converges_with_agent_role`.
//! - Leg 4b: `iroh-rooms-net/tests/join_e2e.rs::agent_bad_capability_secret_join_not_accepted`
//!   and `…::agent_expired_invite_join_not_accepted` — `role = "agent"` mirrors of
//!   the member-role `bad_capability_secret_join_not_accepted` /
//!   `expired_invite_join_not_accepted` proofs, closing IR-0207's online
//!   capability-rejection gap at the Node layer.
//!
//! Neither lives in this always-green, network-free tier.
//!
//! Exhaustive matrices this suite intentionally *references* rather than repeats:
//! the offline AC matrix in `agent_cli.rs`, the role-neutral join failures in
//! `join_cli.rs`, and the coded-line/exit-code contract in `error_taxonomy.rs`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

// ── helpers (mirror agent_cli.rs / error_taxonomy.rs) ──────────────────────────

fn cmd(home: &TempDir) -> Command {
    cmd_at(home.path())
}

fn cmd_at(path: &Path) -> Command {
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
fn extract_ticket(stdout: &str) -> Option<String> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("ticket:") {
            return lines.next().map(|l| l.trim().to_owned());
        }
    }
    None
}

/// Stand up an admin home (identity + room) and mint a **key-bound `agent`-role**
/// ticket for `agent_hex` via the first-class `agent invite` noun. Returns the
/// `roomtkt1…` token — the exact capability the leg-4 rejection tests corrupt.
fn admin_agent_invite_ticket(home_admin: &TempDir, agent_hex: &str) -> String {
    create_identity_named(home_admin, "Alice");
    let room_id = create_room(home_admin);
    let out = cmd(home_admin)
        .args(["agent", "invite", &room_id, agent_hex])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin `agent invite` must succeed to produce a ticket"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_ticket(&stdout).expect("a ticket must follow `ticket:` in `agent invite` output")
}

/// Corrupt a ticket so it **deterministically** fails its trailing 4-byte BLAKE3
/// checksum (`ticket_bad_checksum`) while staying a fully base32-decodable token —
/// a realistic single-character copy-paste garble.
///
/// We flip a character in the **middle** of the base32 body (after the `roomtkt1`
/// prefix). That position always maps to a CBOR *payload* byte, which is the key
/// to determinism given the decode order in `iroh-rooms-core`'s ticket codec:
///   * the **final** body char carries the RFC-4648 canonical zero-padding bits;
///     flipping it can set a padding bit, which a strict decoder rejects as
///     `ticket_bad_base32` (the ~1/32 flake a naive last-char flip exhibits when
///     the minted ticket happens to end in `a`), and
///   * the **first** body char decodes into the version byte, which is validated
///     *before* the checksum, so corrupting it can surface `unsupported_version`.
///
/// A middle char dodges both: changing it alters one payload byte with the version
/// byte and length intact, so the appended checksum no longer matches. Both `a`
/// and `b` are in the lowercase `a-z2-7` alphabet, so the token stays decodable.
fn corrupt_checksum(ticket: &str) -> String {
    let prefix_len = "roomtkt1".len();
    let mut chars: Vec<char> = ticket.chars().collect();
    // Midpoint of the body: a payload byte, far from the version byte and the
    // trailing padding bits.
    let mid = prefix_len + (chars.len() - prefix_len) / 2;
    chars[mid] = if chars[mid] == 'a' { 'b' } else { 'a' };
    chars.into_iter().collect()
}

/// A fixed 64-hex agent identity key (32 raw bytes). `IdentityKey::from_bytes`
/// does not validate curve-point membership, so any well-formed 32-byte hex is a
/// valid invitee key; this value never collides with a CSPRNG-generated identity,
/// so a freshly-created joiner is always a *different* identity than the bound key.
const AGENT_HEX: &str = "0606060606060606060606060606060606060606060606060606060606060606";
/// A second fixed 64-hex key, used as the **human** (`member`-role) invitee in the
/// role-parity tests so the agent and human tickets bind distinct identities.
const MEMBER_HEX: &str = "0404040404040404040404040404040404040404040404040404040404040404";

/// Stand up one admin home + room and mint two tickets from it that differ **only
/// in role**: an `agent`-role ticket (via the `agent invite` noun) bound to
/// `agent_hex`, and a `member`-role (human) ticket (via `room invite --role
/// member`) bound to `member_hex`. Sharing the room and admin isolates the role as
/// the single variable, so any divergence in how the two are later rejected is
/// attributable to the role alone — the premise of the code-identity parity tests.
fn admin_agent_and_member_tickets(
    home_admin: &TempDir,
    agent_hex: &str,
    member_hex: &str,
) -> (String, String) {
    create_identity_named(home_admin, "Alice");
    let room_id = create_room(home_admin);

    let agent_out = cmd(home_admin)
        .args(["agent", "invite", &room_id, agent_hex])
        .output()
        .unwrap();
    assert!(
        agent_out.status.success(),
        "admin `agent invite` must succeed to produce an agent ticket"
    );
    let agent_ticket = extract_ticket(&String::from_utf8_lossy(&agent_out.stdout))
        .expect("a ticket must follow `ticket:` in `agent invite` output");

    let member_out = cmd(home_admin)
        .args([
            "room",
            "invite",
            &room_id,
            "--invitee",
            member_hex,
            "--role",
            "member",
        ])
        .output()
        .unwrap();
    assert!(
        member_out.status.success(),
        "admin `room invite --role member` must succeed to produce a human ticket"
    );
    let member_ticket = extract_ticket(&String::from_utf8_lossy(&member_out.stdout))
        .expect("a ticket must follow `ticket:` in `room invite` output");

    (agent_ticket, member_ticket)
}

/// Extract the `<code>` from the machine-parseable `error[<code>]: …` render line
/// (IR-0110 §5.3), or `None` if the stream carries no coded line. Used to compare
/// the *code* of two rejections while ignoring the human message that follows it
/// (which legitimately differs, e.g. by naming a different bound key).
fn extract_error_code(stream: &str) -> Option<String> {
    let start = stream.find("error[")? + "error[".len();
    let rest = &stream[start..];
    let end = rest.find(']')?;
    Some(rest[..end].to_owned())
}

// ── Leg 1 (AC1): the admin invites an agent by identity key ────────────────────

/// Flow entry point + the setup the leg-4 rejection tests depend on: an admin's
/// `agent invite <ROOM> <AGENT_ID>` mints a `roomtkt1…` capability that decodes to
/// `role == "agent"` bound to the exact agent key, and the agent lands in
/// membership state as `role=agent status=invited`. (Exhaustive AC1 matrix:
/// `agent_cli.rs`; here it is one concise assertion so the suite reads as a whole
/// flow and the leg-4 corruptions operate on a *real* agent ticket.)
#[test]
fn agent_invite_mints_key_bound_agent_ticket() {
    use iroh_rooms_core::ticket::RoomInviteTicket;

    let home = TempDir::new().unwrap();
    let ticket = admin_agent_invite_ticket(&home, AGENT_HEX);

    let decoded: RoomInviteTicket = ticket.parse().expect("minted agent ticket must decode");
    assert_eq!(
        decoded.role, "agent",
        "the minted ticket must be agent-role"
    );
    assert_eq!(
        decoded.invitee_key.to_string(),
        AGENT_HEX,
        "the minted ticket must be key-bound to the agent id"
    );

    // The fold, read back by a separate process, shows the agent invited-not-active.
    let room_id = decoded.room_id.to_string();
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=agent"))
        .stdout(predicate::str::contains("status=invited"))
        .stdout(predicate::str::contains(AGENT_HEX));
}

// ── Leg 3 (AC2): a non-admin cannot invite an agent ────────────────────────────

/// Only the single immutable admin may authorize an agent. A non-admin who holds a
/// copy of the room store but is not the admin cannot mint an agent invite: the
/// request fails with an actionable "admin" message and leaves `rooms.db`
/// byte-for-byte unchanged, so no agent is granted access without the admin's
/// signature. (The gate is an uncoded `bail!` in `invite::invite`, so we assert the
/// human message + store integrity, exactly as `agent_cli.rs` does — the exhaustive
/// backstop — rather than a taxonomy code.)
#[test]
fn non_admin_cannot_invite_an_agent() {
    let home_admin = TempDir::new().unwrap();
    let home_bob = TempDir::new().unwrap();

    // Alice creates the room (admin in home_admin).
    create_identity_named(&home_admin, "Alice");
    let room_id = create_room(&home_admin);

    // Bob has his own identity and a copy of the room store, but is NOT the admin.
    create_identity_named(&home_bob, "Bob");
    std::fs::copy(
        home_admin.path().join("rooms.db"),
        home_bob.path().join("rooms.db"),
    )
    .expect("copy rooms.db into home_bob");
    let db_before = std::fs::metadata(home_bob.path().join("rooms.db")).map_or(0, |m| m.len());

    cmd(&home_bob)
        .args(["agent", "invite", &room_id, AGENT_HEX])
        .assert()
        // The gate is an uncoded `bail!` in `invite::invite` (single immutable
        // admin), so it takes the graceful uncoded fallback: exit 1 with a plain
        // `error:` line — never a coded `error[…]` (which would signal a different,
        // taxonomy-adopted path) and never a silent success. Pinning the exit code
        // guards the "non-admin cannot invite" boundary against an accidental
        // reclassification or a regression that let the invite through.
        .code(1)
        .stderr(predicate::str::contains("admin"))
        .stderr(predicate::str::starts_with("error:"))
        .stderr(predicate::str::contains("error[").not());

    let db_after = std::fs::metadata(home_bob.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db must be unchanged when a non-admin's agent invite is rejected (AC2/AC3)"
    );
}

// ── Leg 4a (AC3): an agent join is rejected without a valid capability ──────────
//
// The genuine new coverage. `gate_join`, the ticket codec, and the key-binding
// pre-check are role-agnostic, so an agent's bad-capability join fails with the
// *identical* code a human's does — proven here through the `agent invite` surface.

/// A structurally corrupt agent ticket (one flipped payload char breaks the trailing
/// BLAKE3 checksum) is rejected at the ticket-decode boundary — exit 5 (Ticket
/// category) with a `ticket_bad_checksum` coded line — **before any network or store
/// IO**, so
/// the would-be agent joiner never persists a membership row (`rooms.db` is never
/// even created). The identical failure a corrupted *human* ticket produces
/// (`error_taxonomy.rs::ticket_bad_checksum_*`), asserted for the agent flow.
#[test]
fn corrupt_agent_ticket_rejected_with_ticket_code_and_no_membership() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_agent_invite_ticket(&home_admin, AGENT_HEX);
    let corrupted = corrupt_checksum(&ticket);

    // A fresh joiner home with its own identity (never touched, since decode fails first).
    let home_joiner = TempDir::new().unwrap();
    create_identity_named(&home_joiner, "Joiner");

    cmd(&home_joiner)
        .args(["room", "join", &corrupted])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("error[ticket_bad_checksum]:"));

    assert!(
        !home_joiner.path().join("rooms.db").exists(),
        "a corrupt agent ticket must grant no membership: the store is never opened"
    );
}

/// A truncated agent ticket (a chunk of the base32 body chopped off) fails closed in
/// the Ticket category (exit 5, a `ticket_*` code) — the codec never decodes a
/// partial paste into a usable capability. Asserts the exit category + the `ticket_`
/// family prefix (the exact arm depends on where the truncation lands: bad base32
/// length, a short payload, or a checksum mismatch), and grants no membership.
#[test]
fn truncated_agent_ticket_rejected_with_ticket_category() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_agent_invite_ticket(&home_admin, AGENT_HEX);
    // Drop the last 6 chars: enough to break both the checksum and, typically, the
    // base32 block alignment — a garbled/short paste that must fail closed.
    let truncated = &ticket[..ticket.len() - 6];

    let home_joiner = TempDir::new().unwrap();
    create_identity_named(&home_joiner, "Joiner");

    cmd(&home_joiner)
        .args(["room", "join", truncated])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("error[ticket_"));

    assert!(
        !home_joiner.path().join("rooms.db").exists(),
        "a truncated agent ticket must grant no membership: the store is never opened"
    );
}

/// A structurally *valid* agent ticket bound to `AGENT_HEX`, redeemed from a home
/// whose identity is a different (freshly generated) key, is rejected by the
/// key-binding pre-check — exit 3 (Auth), a `wrong_identity` coded line — **before
/// any dial**. The message names the ticket's bound agent key so the operator knows
/// whose invite to request, and no membership is persisted. This is the offline,
/// agent-flavored mirror of `join_cli.rs::join_wrong_identity_*`.
#[test]
fn wrong_identity_agent_ticket_rejected_pre_io_names_bound_key() {
    let home_admin = TempDir::new().unwrap();
    let ticket = admin_agent_invite_ticket(&home_admin, AGENT_HEX);

    // The joiner is a real, distinct identity — not the agent key the ticket binds.
    let home_joiner = TempDir::new().unwrap();
    create_identity_named(&home_joiner, "Mallory");

    let out = cmd(&home_joiner)
        .args(["room", "join", &ticket])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(3),
        "a wrong-identity agent join must exit in the Auth category (3)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[wrong_identity]:"),
        "must render the wrong_identity coded line; got: {stderr}"
    );
    assert!(
        stderr.contains(AGENT_HEX),
        "the error must name the ticket's bound agent key so the operator can request the right invite; got: {stderr}"
    );
    assert!(
        !home_joiner.path().join("rooms.db").exists(),
        "a wrong-identity agent join fails pre-IO and must grant no membership"
    );
}

/// AC3 secret hygiene through the agent surface: a rejected (corrupt) agent ticket
/// must never echo the raw token — whose base32 body embeds the capability secret —
/// nor the secret's hex, on any stream. A redacted, *coded* `ticket_*` line is still
/// surfaced, so the no-leak guarantee is not vacuously satisfied by an empty error.
/// Mirrors `error_taxonomy.rs::corrupted_ticket_never_echoes_token_or_secret` for
/// the agent flow.
#[test]
fn corrupt_agent_ticket_never_echoes_token_or_capability_secret() {
    use iroh_rooms_core::ticket::RoomInviteTicket;

    let home_admin = TempDir::new().unwrap();
    let ticket = admin_agent_invite_ticket(&home_admin, AGENT_HEX);

    // Recover the capability secret carried in the (still valid) token — test-side
    // only — so we can assert it is never rendered on a failure path.
    let decoded: RoomInviteTicket = ticket.parse().expect("minted agent ticket must decode");
    let secret_hex = hex::encode(decoded.capability_secret);
    let corrupted = corrupt_checksum(&ticket);

    let home_joiner = TempDir::new().unwrap();
    create_identity_named(&home_joiner, "Joiner");

    let out = cmd(&home_joiner)
        .args(["room", "join", &corrupted])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(5),
        "a corrupt agent ticket must fail in the Ticket exit category"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !combined.contains(&secret_hex),
        "an agent ticket error must not echo the capability secret hex"
    );
    assert!(
        !combined.contains(&corrupted),
        "an agent ticket error must not echo the (corrupted) raw token"
    );
    assert!(
        !combined.contains(&ticket),
        "an agent ticket error must not echo the original raw token"
    );
    assert!(
        stderr.contains("error[ticket_"),
        "a redacted, coded ticket error must still be rendered; got: {stderr}"
    );
}

// ── Leg 4a (AC3 core): the agent is gated *identically* to a human ─────────────
//
// IR-0207's load-bearing guarantee is that agent join uses "the same capability
// verification as a human peer". The ticket codec and the key-binding pre-check
// never branch on `role`, so the two flows *should* be byte-identical on the
// rejection path. These tests assert that sameness rather than assume it: they
// mint an `agent` ticket and a `member` (human) ticket that differ only in role,
// corrupt/misredeem each identically, and require the resulting IR-0110 code +
// exit category to match. A future refactor that special-cased the `agent` role
// (spec R5) would diverge one code and fail here — this is the durable guard.

/// Code-identity on the ticket-decode boundary: an `agent` ticket and a `member`
/// ticket, corrupted by the identical one-character flip, are rejected with the
/// **same** IR-0110 code (`ticket_bad_checksum`) and the **same** exit category
/// (5). The rejection does not depend on the invited role — exactly the "same
/// verification as a human" acceptance criterion, asserted at the codec boundary.
#[test]
fn agent_and_human_corrupt_ticket_reject_with_identical_code() {
    let home_admin = TempDir::new().unwrap();
    let (agent_ticket, member_ticket) =
        admin_agent_and_member_tickets(&home_admin, AGENT_HEX, MEMBER_HEX);
    let corrupt_agent = corrupt_checksum(&agent_ticket);
    let corrupt_member = corrupt_checksum(&member_ticket);

    // Each corrupted ticket is redeemed from its own fresh joiner home; decode
    // fails first, so the identity is never consulted and no store is opened.
    let home_agent_joiner = TempDir::new().unwrap();
    create_identity_named(&home_agent_joiner, "AgentJoiner");
    let agent_out = cmd(&home_agent_joiner)
        .args(["room", "join", &corrupt_agent])
        .output()
        .unwrap();

    let home_member_joiner = TempDir::new().unwrap();
    create_identity_named(&home_member_joiner, "HumanJoiner");
    let member_out = cmd(&home_member_joiner)
        .args(["room", "join", &corrupt_member])
        .output()
        .unwrap();

    assert_eq!(
        agent_out.status.code(),
        member_out.status.code(),
        "a corrupt agent ticket and a corrupt human ticket must share an exit category"
    );
    assert_eq!(
        agent_out.status.code(),
        Some(5),
        "both must fail in the Ticket exit category"
    );

    let agent_code = extract_error_code(&String::from_utf8_lossy(&agent_out.stderr));
    let member_code = extract_error_code(&String::from_utf8_lossy(&member_out.stderr));
    assert_eq!(
        agent_code, member_code,
        "the agent and human ticket-corruption rejections must carry the identical IR-0110 code"
    );
    assert_eq!(
        agent_code.as_deref(),
        Some("ticket_bad_checksum"),
        "a single-char flip breaks the trailing checksum for both roles"
    );

    // Neither role's failing decode opens a store (the join is rejected pre-IO).
    assert!(
        !home_agent_joiner.path().join("rooms.db").exists()
            && !home_member_joiner.path().join("rooms.db").exists(),
        "a corrupt ticket grants no membership regardless of role"
    );
}

/// Code-identity on the key-binding pre-check: a structurally valid `agent` ticket
/// and a structurally valid `member` ticket, each redeemed from a **different**
/// (freshly generated) identity than the one it binds, are rejected with the
/// **same** IR-0110 code (`wrong_identity`) and the **same** exit category (3),
/// before any dial. The join pre-check keys on the bound identity, not the role.
#[test]
fn agent_and_human_wrong_identity_reject_with_identical_code() {
    let home_admin = TempDir::new().unwrap();
    let (agent_ticket, member_ticket) =
        admin_agent_and_member_tickets(&home_admin, AGENT_HEX, MEMBER_HEX);

    // Two distinct, freshly-generated joiners — neither is the key its ticket binds.
    let home_agent_joiner = TempDir::new().unwrap();
    create_identity_named(&home_agent_joiner, "Mallory");
    let agent_out = cmd(&home_agent_joiner)
        .args(["room", "join", &agent_ticket])
        .output()
        .unwrap();

    let home_member_joiner = TempDir::new().unwrap();
    create_identity_named(&home_member_joiner, "Trudy");
    let member_out = cmd(&home_member_joiner)
        .args(["room", "join", &member_ticket])
        .output()
        .unwrap();

    assert_eq!(
        agent_out.status.code(),
        member_out.status.code(),
        "a wrong-identity agent join and a wrong-identity human join must share an exit category"
    );
    assert_eq!(
        agent_out.status.code(),
        Some(3),
        "both must fail in the Auth exit category"
    );

    let agent_code = extract_error_code(&String::from_utf8_lossy(&agent_out.stderr));
    let member_code = extract_error_code(&String::from_utf8_lossy(&member_out.stderr));
    assert_eq!(
        agent_code, member_code,
        "the agent and human wrong-identity rejections must carry the identical IR-0110 code"
    );
    assert_eq!(
        agent_code.as_deref(),
        Some("wrong_identity"),
        "the key-binding pre-check rejects both roles with wrong_identity"
    );

    // Each error names its own ticket's bound key (the agent one names the agent
    // key; the human one names the human key) — the pre-check is per-ticket, and
    // the two messages legitimately differ while the *code* is identical.
    assert!(
        String::from_utf8_lossy(&agent_out.stderr).contains(AGENT_HEX),
        "the agent-ticket rejection must name the bound agent key"
    );
    assert!(
        String::from_utf8_lossy(&member_out.stderr).contains(MEMBER_HEX),
        "the human-ticket rejection must name the bound human key"
    );

    assert!(
        !home_agent_joiner.path().join("rooms.db").exists()
            && !home_member_joiner.path().join("rooms.db").exists(),
        "a wrong-identity join fails pre-IO and grants no membership regardless of role"
    );
}
