//! CLI integration tests for `room create` and `room members` (issue #17 / IR-0102).
//!
//! Each test gets its own temp directory via `--data-dir` so tests are fully
//! isolated even when run in parallel. `IROH_ROOMS_HOME` is removed from the
//! environment in all tests to prevent interference from the developer's shell.
//!
//! Coverage map (spec §11):
//!   §11 test 1 — `room_create_exits_zero_and_prints_room_id` + `room_create_room_id_is_valid_blake3_hex`
//!   §11 test 2 — `room_members_shows_creator_as_admin_and_active`
//!   §11 test 3 — `room_members_in_separate_process_reads_from_db` (AC5 restart)
//!   §11 test 4 — `room_members_reports_room_id_consistent_with_create` (AC3 end-to-end)
//!              + `room_members_unknown_room_id_exits_nonzero`
//!   §11 test 5 — `room_create_without_identity_exits_nonzero_with_hint`
//!   §11 test 6 — `room_create_empty_name_*` / `room_create_overlong_name_*` / `room_create_control_char_name_*`
//!   §11 test 7 — `room_create_does_not_expose_secret_seeds`
//!   §11 test 8 — `two_room_creates_produce_distinct_room_ids`
//!   §11 test 9 — `data_dir_flag_isolates_rooms`
//!   AC4 extra  — `room_create_admin_matches_identity_id` + `room_members_admin_matches_identity_id`
//!   AC1/format — `room_create_output_contains_next_step_hint`
//!   malformed  — `room_members_malformed_room_id_exits_nonzero`

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

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a `Command` for the `iroh-rooms` binary pointed at `home` via
/// `--data-dir`.  `IROH_ROOMS_HOME` is cleared so the flag is the sole source.
fn cmd(home: &TempDir) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(home.path());
    c
}

/// Same as [`cmd`] but accepts a plain `&std::path::Path` — needed when a
/// single test needs to build commands for two distinct temp directories.
fn cmd_at(path: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME").arg("--data-dir").arg(path);
    c
}

/// Run `identity create --name Alice` in `home`. Panics if it fails (a
/// prerequisite for most room tests).
fn create_identity(home: &TempDir) {
    cmd(home)
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();
}

/// Extract the value of a `key: value` line from CLI text output.
fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            return Some(rest.trim());
        }
    }
    None
}

// ── room create: basic success (AC1) ─────────────────────────────────────────

/// AC1: command exits 0 and emits a `room_id:` line, plus creates `rooms.db`.
#[test]
fn room_create_exits_zero_and_prints_room_id() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "create", "Build Room"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created room"))
        .stdout(predicate::str::contains("room_id:"))
        .stdout(predicate::str::contains("blake3:"));
    assert!(
        home.path().join("rooms.db").exists(),
        "rooms.db must be created after a successful room create"
    );
}

/// The `room_id` in the output must be a well-formed `blake3:<64-hex>` string.
#[test]
fn room_create_room_id_is_valid_blake3_hex() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let output = cmd(&home)
        .args(["room", "create", "Build Room"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let room_id = extract_field(&stdout, "room_id").expect("room_id must appear in output");

    assert!(
        room_id.starts_with("blake3:"),
        "room_id must start with 'blake3:' but got: {room_id}"
    );
    let hex_part = room_id.strip_prefix("blake3:").unwrap();
    assert_eq!(
        hex_part.len(),
        64,
        "room_id hex part must be exactly 64 chars"
    );
    assert!(
        hex_part
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "room_id hex must be lowercase: {hex_part}"
    );
}

/// `room create` output must also print the admin `identity_id` and a next-step hint.
#[test]
fn room_create_output_contains_admin_and_next_step_hint() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "create", "Build Room"])
        .assert()
        .success()
        .stdout(predicate::str::contains("admin:"))
        .stdout(predicate::str::contains("room members"));
}

// ── room create: admin matches identity (AC4) ─────────────────────────────────

/// The `admin:` field in `room create` output must equal the creator's
/// `identity_id` from `identity show`.
#[test]
fn room_create_admin_matches_identity_id() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let show_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    assert!(show_out.status.success());
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    let identity_id =
        extract_field(&show_stdout, "identity_id").expect("identity_id in show output");

    let create_out = cmd(&home)
        .args(["room", "create", "My Room"])
        .output()
        .unwrap();
    assert!(create_out.status.success());
    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let admin_id = extract_field(&create_stdout, "admin").expect("admin in create output");

    assert_eq!(
        admin_id, identity_id,
        "admin in create output must equal the creator's identity_id"
    );
}

// ── room members: creator is admin & active (AC2/AC4) ────────────────────────

/// AC4: `room members` must report the creator as admin with `role=admin` and
/// `status=active`.
#[test]
fn room_members_shows_creator_as_admin_and_active() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let create_out = cmd(&home)
        .args(["room", "create", "Build Room"])
        .output()
        .unwrap();
    assert!(create_out.status.success());
    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let room_id = extract_field(&create_stdout, "room_id").expect("room_id in create output");

    cmd(&home)
        .args(["room", "members", room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("admin:"))
        .stdout(predicate::str::contains("role=admin"))
        .stdout(predicate::str::contains("status=active"));
}

/// The `admin:` line in `room members` must equal the creator's `identity_id`.
#[test]
fn room_members_admin_matches_identity_id() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let show_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    let identity_id = extract_field(&show_stdout, "identity_id")
        .expect("identity_id in show output")
        .to_owned();

    let create_out = cmd(&home)
        .args(["room", "create", "My Room"])
        .output()
        .unwrap();
    assert!(create_out.status.success());
    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let room_id = extract_field(&create_stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();

    let members_out = cmd(&home)
        .args(["room", "members", &room_id])
        .output()
        .unwrap();
    assert!(members_out.status.success());
    let members_stdout = String::from_utf8_lossy(&members_out.stdout);
    let admin_id = extract_field(&members_stdout, "admin").expect("admin in members output");

    assert_eq!(
        admin_id, identity_id,
        "admin in members output must equal the creator's identity_id"
    );
}

// ── room survives CLI restart (AC5) ──────────────────────────────────────────

/// AC5: a **separate** process invocation of `room members` (simulating a CLI
/// restart) must return the creator as admin — the state comes from `rooms.db`,
/// not from in-process memory.
#[test]
fn room_members_in_separate_process_reads_from_db() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let create_out = cmd(&home)
        .args(["room", "create", "Persistent Room"])
        .output()
        .unwrap();
    assert!(create_out.status.success());
    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let room_id = extract_field(&create_stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();

    // cmd() spawns a new process each time — this simulates a restart.
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("admin:"))
        .stdout(predicate::str::contains("role=admin"))
        .stdout(predicate::str::contains("status=active"));
}

// ── room id recomputes end-to-end (AC3) ──────────────────────────────────────

/// AC3 end-to-end: `room members` re-validates the stored genesis event, which
/// recomputes the `room_id` from the signed fields. The `room:` field in members
/// output must match the `room_id:` printed by `create`.
#[test]
fn room_members_reports_room_id_consistent_with_create() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let create_out = cmd(&home)
        .args(["room", "create", "Room"])
        .output()
        .unwrap();
    assert!(create_out.status.success());
    let create_stdout = String::from_utf8_lossy(&create_out.stdout);
    let room_id_from_create = extract_field(&create_stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();

    let members_out = cmd(&home)
        .args(["room", "members", &room_id_from_create])
        .output()
        .unwrap();
    assert!(members_out.status.success());
    let members_stdout = String::from_utf8_lossy(&members_out.stdout);
    let room_id_from_members =
        extract_field(&members_stdout, "room").expect("room: in members output");

    assert_eq!(
        room_id_from_create, room_id_from_members,
        "room_id must be stable between create and members"
    );
}

/// AC3 negative: `room members` with a well-formed but unknown room id must
/// exit non-zero with an actionable error mentioning the missing room.
#[test]
fn room_members_unknown_room_id_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // Create one room so rooms.db exists; the unknown id is a different room.
    cmd(&home)
        .args(["room", "create", "Some Room"])
        .assert()
        .success();

    let unknown_id = format!("blake3:{}", "ab".repeat(32));
    cmd(&home)
        .args(["room", "members", &unknown_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));
}

// ── no identity → actionable error (spec §11 test 5) ─────────────────────────

/// Running `room create` with no prior `identity create` must exit non-zero and
/// hint at `identity create`.
#[test]
fn room_create_without_identity_exits_nonzero_with_hint() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["room", "create", "X"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("identity create"));
}

/// Running `room members` with no prior identity must also exit non-zero.
#[test]
fn room_members_without_identity_exits_nonzero() {
    let home = TempDir::new().unwrap();
    let fake_id = format!("blake3:{}", "cc".repeat(32));
    cmd(&home)
        .args(["room", "members", &fake_id])
        .assert()
        .failure();
}

// ── name validation (spec §11 test 6 / D7) ───────────────────────────────────

/// Empty name must be rejected before any IO: no `rooms.db` created.
#[test]
fn room_create_empty_name_exits_nonzero_and_writes_no_db() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home).args(["room", "create", ""]).assert().failure();
    assert!(
        !home.path().join("rooms.db").exists(),
        "rooms.db must not be created when the room name is empty"
    );
}

/// Name over 128 bytes must be rejected.
#[test]
fn room_create_overlong_name_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let long_name = "a".repeat(129);
    cmd(&home)
        .args(["room", "create", &long_name])
        .assert()
        .failure();
}

/// A 128-byte name (exactly the limit) must be accepted.
#[test]
fn room_create_exactly_max_length_name_succeeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let max_name = "a".repeat(128);
    cmd(&home)
        .args(["room", "create", &max_name])
        .assert()
        .success();
}

/// Name containing a newline (control character) must be rejected.
#[test]
fn room_create_name_with_newline_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "create", "Build\nRoom"])
        .assert()
        .failure();
}

/// Name containing a tab (control character) must be rejected.
#[test]
fn room_create_name_with_tab_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "create", "Build\tRoom"])
        .assert()
        .failure();
}

/// A valid Unicode name within the byte limit must succeed.
#[test]
fn room_create_unicode_name_within_limit_succeeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "create", "Salle de réunion — café"])
        .assert()
        .success();
}

/// Bad name must not persist a genesis event: if there was a prior room the
/// event count must be unchanged; if no db existed it must remain absent.
#[test]
fn room_create_bad_name_does_not_persist_event() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // Create one valid room first so rooms.db exists.
    cmd(&home)
        .args(["room", "create", "Valid Room"])
        .assert()
        .success();
    let db_before = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());

    // Now attempt an invalid create — must fail.
    cmd(&home).args(["room", "create", ""]).assert().failure();

    let db_after = std::fs::metadata(home.path().join("rooms.db")).map_or(0, |m| m.len());
    assert_eq!(
        db_before, db_after,
        "rooms.db size must not change when room create is rejected for bad name"
    );
}

// ── secrets never leak (spec §11 test 7 / R3) ────────────────────────────────

/// The raw secret seeds stored in `identity.secret` must never appear in stdout
/// or stderr of `room create`.
#[test]
fn room_create_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret")).unwrap();
    let secret_v: serde_json::Value = serde_json::from_str(&secret_raw).unwrap();
    let identity_seed = secret_v["identity_secret"].as_str().unwrap().to_owned();
    let device_seed = secret_v["device_secret"].as_str().unwrap().to_owned();

    let output = cmd(&home)
        .args(["room", "create", "Secure Room"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stdout.contains(&identity_seed),
        "room create stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(&identity_seed),
        "room create stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(&device_seed),
        "room create stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(&device_seed),
        "room create stderr must not contain the device secret seed"
    );
}

// ── two rooms are independent (spec §11 test 8) ──────────────────────────────

/// Two `room create` invocations in the same home must produce two distinct
/// `room_id`s, each resolvable by `room members` with the creator as admin.
#[test]
fn two_room_creates_produce_distinct_room_ids() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let out_a = cmd(&home)
        .args(["room", "create", "Room A"])
        .output()
        .unwrap();
    assert!(out_a.status.success());
    let stdout_a = String::from_utf8_lossy(&out_a.stdout);
    let id_a = extract_field(&stdout_a, "room_id")
        .expect("room_id in first create output")
        .to_owned();

    let out_b = cmd(&home)
        .args(["room", "create", "Room B"])
        .output()
        .unwrap();
    assert!(out_b.status.success());
    let stdout_b = String::from_utf8_lossy(&out_b.stdout);
    let id_b = extract_field(&stdout_b, "room_id")
        .expect("room_id in second create output")
        .to_owned();

    assert_ne!(
        id_a, id_b,
        "two room creates must yield distinct room_ids (nonce must differ)"
    );

    // Both rooms must be independently resolvable.
    cmd(&home)
        .args(["room", "members", &id_a])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=admin"));
    cmd(&home)
        .args(["room", "members", &id_b])
        .assert()
        .success()
        .stdout(predicate::str::contains("role=admin"));
}

// ── --data-dir isolation (spec §11 test 9) ────────────────────────────────────

/// `room create` under `--data-dir <A>` must write `A/rooms.db` and be
/// invisible to a `room members` call pointed at a different directory `<B>`.
#[test]
fn data_dir_flag_isolates_rooms() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();

    // Both directories need their own identity.
    cmd_at(home_a.path())
        .args(["identity", "create", "--name", "Alice"])
        .assert()
        .success();
    cmd_at(home_b.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();

    // Create a room in home_a.
    let out = cmd_at(home_a.path())
        .args(["room", "create", "Alice's Room"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let room_id = extract_field(&stdout, "room_id")
        .expect("room_id in create output")
        .to_owned();

    // rooms.db must be in home_a, not in home_b.
    assert!(
        home_a.path().join("rooms.db").exists(),
        "rooms.db must be in home_a"
    );
    assert!(
        !home_b.path().join("rooms.db").exists(),
        "rooms.db must NOT be written to home_b"
    );

    // `room members` in home_b must fail (room is in home_a's store).
    cmd_at(home_b.path())
        .args(["room", "members", &room_id])
        .assert()
        .failure();

    // `room members` in home_a must succeed.
    cmd_at(home_a.path())
        .args(["room", "members", &room_id])
        .assert()
        .success();
}

// ── malformed room id (spec §8) ───────────────────────────────────────────────

/// A syntactically malformed room id (not `blake3:<hex>`) must produce a
/// non-zero exit with an actionable error message.
#[test]
fn room_members_malformed_room_id_exits_nonzero_with_hint() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["room", "members", "not-a-valid-room-id"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid room id"));
}

/// A `blake3:` prefix with wrong-length hex must also be rejected.
#[test]
fn room_members_truncated_room_id_hex_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // 62 hex chars instead of 64 — one byte short.
    cmd(&home)
        .args([
            "room",
            "members",
            "blake3:aabbccddeeff00112233445566778899aabbccddeeff0011223344556677",
        ])
        .assert()
        .failure();
}

// ── room members --json (AC2 / AC4 — IR-0106) ─────────────────────────────────

/// Helper: create a room and return its `room_id` string.
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

/// AC4: `room members --json` exits 0 and its output parses as a JSON object
/// with the required top-level fields.
#[test]
fn room_members_json_exits_zero_and_parses_as_json_object() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "room members --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json output must be valid JSON");
    assert!(parsed.is_object(), "--json output must be a JSON object");
    assert!(
        parsed["room"].is_string(),
        "JSON must have a 'room' string field"
    );
    assert!(
        parsed["members"].is_array(),
        "JSON must have a 'members' array field"
    );
}

/// The `room` field in the JSON output must match the `room_id` from `room create`.
#[test]
fn room_members_json_room_field_matches_create_room_id() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["room"].as_str(),
        Some(room_id.as_str()),
        "JSON 'room' field must match the room_id from room create"
    );
}

/// The `admin` field in the JSON output must equal the creator's `identity_id`.
#[test]
fn room_members_json_admin_field_matches_identity_id() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let show_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    let identity_id = extract_field(&show_stdout, "identity_id")
        .expect("identity_id in identity show output")
        .to_owned();

    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["admin"].as_str(),
        Some(identity_id.as_str()),
        "JSON 'admin' field must equal the creator's identity_id"
    );
}

/// AC2 + AC4: the creator must appear in `members` with `role="admin"`,
/// `status="active"`, and `is_admin=true`.
#[test]
fn room_members_json_creator_has_admin_role_active_status_is_admin_true() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let members = parsed["members"].as_array().unwrap();
    assert_eq!(members.len(), 1, "fresh room must have exactly one member");
    let m = &members[0];
    assert_eq!(
        m["role"].as_str(),
        Some("admin"),
        "creator role must be admin"
    );
    assert_eq!(
        m["status"].as_str(),
        Some("active"),
        "creator status must be active"
    );
    assert_eq!(
        m["is_admin"].as_bool(),
        Some(true),
        "creator is_admin must be true"
    );
    assert!(
        m["identity_id"].is_string(),
        "each member must have an identity_id string"
    );
}

/// The `identity_id` in the members array must equal the creator's `identity_id`.
#[test]
fn room_members_json_member_identity_id_matches_creator() {
    let home = TempDir::new().unwrap();
    create_identity(&home);

    let show_out = cmd(&home).args(["identity", "show"]).output().unwrap();
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    let identity_id = extract_field(&show_stdout, "identity_id")
        .expect("identity_id in identity show output")
        .to_owned();

    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let members = parsed["members"].as_array().unwrap();
    assert_eq!(
        members[0]["identity_id"].as_str(),
        Some(identity_id.as_str()),
        "member identity_id must match the creator's identity_id"
    );
}

/// JSON output is consistent with the text output: both report the same admin.
#[test]
fn room_members_json_and_text_report_same_admin() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let text_out = cmd(&home)
        .args(["room", "members", &room_id])
        .output()
        .unwrap();
    assert!(text_out.status.success());
    let text_stdout = String::from_utf8_lossy(&text_out.stdout);
    let text_admin = extract_field(&text_stdout, "admin").expect("admin: in text output");

    let json_out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let json_stdout = String::from_utf8_lossy(&json_out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(json_stdout.trim()).unwrap();
    let json_admin = parsed["admin"].as_str().expect("admin field in JSON");

    assert_eq!(
        text_admin, json_admin,
        "text and JSON output must report the same admin identity_id"
    );
}

/// AC3 negative: `--json` with an unknown (but valid-format) room id must exit non-zero.
#[test]
fn room_members_json_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    create_room(&home); // so rooms.db exists
    let unknown_id = format!("blake3:{}", "cd".repeat(32));
    cmd(&home)
        .args(["room", "members", &unknown_id, "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));
}

// ── AC3: removed / left member representation ─────────────────────────────────
//
// These tests seed a `rooms.db` directly using the core event builders (no CLI
// invite/join needed) and verify that `room members` correctly distinguishes
// `status=left` (voluntary self-departure) from `status=removed` (admin-removal).
// Admin-removal dominates a concurrent self-leave (D5). The seeded directory has
// no `identity.secret` — the offline `room members` does not require one (spec D7).

const AC3M_ADMIN_SEED: [u8; 32] = [0x20; 32];
const AC3M_ADMIN_DEV_SEED: [u8; 32] = [0x21; 32];
const AC3M_MEMBER_SEED: [u8; 32] = [0x24; 32];
const AC3M_MEMBER_DEV_SEED: [u8; 32] = [0x25; 32];
const AC3M_ROOM_NONCE: [u8; 16] = [0xcc; 16];
const AC3M_INVITE_ID: [u8; 16] = [0xdb; 16];
const AC3M_CAP_SECRET: [u8; 16] = [0x5f; 16];
const AC3M_BASE_TS: u64 = 1_750_000_001_000;

#[derive(Copy, Clone)]
enum MembersDeparture {
    Left,
    Removed,
    ConcurrentLeftAndRemoved,
}

/// Seed `home/rooms.db` with genesis → invite → join → departure and return the
/// room id string for use with the CLI.
#[allow(clippy::too_many_lines)]
fn seed_members_departed(home: &TempDir, departure: MembersDeparture) -> String {
    let admin_id = SigningKey::from_seed(&AC3M_ADMIN_SEED);
    let admin_dev = SigningKey::from_seed(&AC3M_ADMIN_DEV_SEED);
    let member_id = SigningKey::from_seed(&AC3M_MEMBER_SEED);
    let member_dev = SigningKey::from_seed(&AC3M_MEMBER_DEV_SEED);

    let room_id = signed::derive_room_id(&admin_id.identity_key(), &AC3M_ROOM_NONCE, AC3M_BASE_TS);
    let ctx = ValidationContext::for_room(room_id);
    let db_path = home.path().join("rooms.db");
    let mut store = EventStore::open(&db_path).expect("open store");

    // Genesis
    let genesis_wire = build_room_created(
        &admin_id,
        &admin_dev,
        "AC3 Members Room",
        &AC3M_ROOM_NONCE,
        AC3M_BASE_TS,
    );
    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("validate genesis");
    let genesis_ev_id = genesis_v.event_id;
    store.insert(&genesis_v).expect("insert genesis");

    // Invite (Bob)
    let cap_hash = capability_hash(&room_id, &AC3M_INVITE_ID, &AC3M_CAP_SECRET);
    let invite_wire = build_member_invited(
        &admin_id,
        &admin_dev,
        &room_id,
        &AC3M_INVITE_ID,
        &cap_hash,
        "member",
        &member_id.identity_key(),
        None,
        None,
        &[genesis_ev_id],
        AC3M_BASE_TS + 1_000,
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
        &AC3M_INVITE_ID,
        &AC3M_CAP_SECRET,
        "member",
        binding,
        Some("Bob"),
        &[invite_ev_id],
        AC3M_BASE_TS + 2_000,
    );
    let join_v = validate_wire_bytes(&join_wire.to_bytes(), &ctx).expect("validate join");
    let join_ev_id = join_v.event_id;
    store.insert(&join_v).expect("insert join");

    // Departure
    match departure {
        MembersDeparture::Left => {
            let left_wire = build_member_left(
                &member_id,
                &member_dev,
                &room_id,
                None,
                &[join_ev_id],
                AC3M_BASE_TS + 3_000,
            );
            let left_v = validate_wire_bytes(&left_wire.to_bytes(), &ctx).expect("validate left");
            store.insert(&left_v).expect("insert left");
        }
        MembersDeparture::Removed => {
            let removed_wire = build_member_removed(
                &admin_id,
                &admin_dev,
                &room_id,
                &member_id.identity_key(),
                None,
                None,
                &[join_ev_id],
                AC3M_BASE_TS + 3_000,
            );
            let removed_v =
                validate_wire_bytes(&removed_wire.to_bytes(), &ctx).expect("validate removed");
            store.insert(&removed_v).expect("insert removed");
        }
        MembersDeparture::ConcurrentLeftAndRemoved => {
            // Both events cite the join as parent (concurrent departures).
            let left_wire = build_member_left(
                &member_id,
                &member_dev,
                &room_id,
                None,
                &[join_ev_id],
                AC3M_BASE_TS + 3_000,
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
                AC3M_BASE_TS + 3_000,
            );
            let removed_v =
                validate_wire_bytes(&removed_wire.to_bytes(), &ctx).expect("validate removed");
            store.insert(&removed_v).expect("insert removed");
        }
    }

    room_id.to_string()
}

/// AC3: `room members` text output shows `status=left` for a voluntarily departed member.
#[test]
fn room_members_voluntary_departure_shows_status_left() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_departed(&home, MembersDeparture::Left);
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=left"));
}

/// AC3: `room members` text output shows `status=removed` for an admin-removed member.
#[test]
fn room_members_admin_removal_shows_status_removed() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_departed(&home, MembersDeparture::Removed);
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=removed"));
}

/// AC3: `room members` text output does not omit the departed member (shown, not silent).
#[test]
fn room_members_departed_member_is_shown_not_omitted() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_departed(&home, MembersDeparture::Left);
    let out = cmd(&home)
        .args(["room", "members", &room_id])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The members list must have 2 entries: the admin (active) and the departed member.
    let member_lines = stdout.lines().filter(|l| l.starts_with("member:")).count();
    assert_eq!(
        member_lines, 2,
        "both the admin and the departed member must appear in `room members` output; \
         stdout:\n{stdout}"
    );
}

/// AC3 + AC4: `room members --json` has `"status":"left"` for a voluntarily departed member.
#[test]
fn room_members_json_voluntary_departure_has_left_status() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_departed(&home, MembersDeparture::Left);
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "room members --json must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let members = parsed["members"].as_array().expect("members array");
    let departed = members
        .iter()
        .find(|m| m["status"].as_str() == Some("left"))
        .expect("must find a member with status=left");
    assert_eq!(
        departed["role"].as_str(),
        Some("member"),
        "the departed member must have role=member"
    );
    assert_eq!(
        departed["is_admin"].as_bool(),
        Some(false),
        "the departed member must not be admin"
    );
}

/// AC3 + AC4: `room members --json` has `"status":"removed"` for an admin-removed member.
#[test]
fn room_members_json_admin_removal_has_removed_status() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_departed(&home, MembersDeparture::Removed);
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let members = parsed["members"].as_array().expect("members array");
    let departed = members
        .iter()
        .find(|m| m["status"].as_str() == Some("removed"))
        .expect("must find a member with status=removed");
    assert_eq!(departed["role"].as_str(), Some("member"));
}

/// AC3 D5 dominance: when both a self-leave and a concurrent admin-removal target the
/// same member, `room members --json` must show `"status":"removed"`, not `"left"`.
#[test]
fn room_members_json_admin_removal_dominates_concurrent_self_leave() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_departed(&home, MembersDeparture::ConcurrentLeftAndRemoved);
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let members = parsed["members"].as_array().expect("members array");
    let member_id_hex = SigningKey::from_seed(&AC3M_MEMBER_SEED)
        .identity_key()
        .to_string();
    let bob = members
        .iter()
        .find(|m| m["identity_id"].as_str() == Some(member_id_hex.as_str()))
        .expect("Bob must appear in the members list");
    assert_eq!(
        bob["status"].as_str(),
        Some("removed"),
        "admin-removal must dominate a concurrent self-leave (D5); Bob's status must be \
         'removed', not 'left'; member JSON: {bob:#?}"
    );
}

// ── AC3: invited member representation ───────────────────────────────────────
//
// These tests seed a `rooms.db` with a genesis → invite chain (no join) and
// verify that `room members` (text and JSON) shows the invited-but-not-joined
// member with `status=invited`. No `identity.secret` is created because offline
// `room members` does not require one (spec D7).

const AC3I_ADMIN_SEED: [u8; 32] = [0x30; 32];
const AC3I_ADMIN_DEV_SEED: [u8; 32] = [0x31; 32];
const AC3I_MEMBER_SEED: [u8; 32] = [0x34; 32];
const AC3I_ROOM_NONCE: [u8; 16] = [0xee; 16];
const AC3I_INVITE_ID: [u8; 16] = [0xdc; 16];
const AC3I_CAP_SECRET: [u8; 16] = [0x60; 16];
const AC3I_BASE_TS: u64 = 1_750_000_002_000;

/// Seed `home/rooms.db` with genesis → invite (no join). Returns the room id string.
fn seed_members_invited(home: &TempDir) -> String {
    let admin_id = SigningKey::from_seed(&AC3I_ADMIN_SEED);
    let admin_dev = SigningKey::from_seed(&AC3I_ADMIN_DEV_SEED);
    let member_id = SigningKey::from_seed(&AC3I_MEMBER_SEED);

    let room_id = signed::derive_room_id(&admin_id.identity_key(), &AC3I_ROOM_NONCE, AC3I_BASE_TS);
    let ctx = ValidationContext::for_room(room_id);
    let db_path = home.path().join("rooms.db");
    let mut store = EventStore::open(&db_path).expect("open store");

    let genesis_wire = build_room_created(
        &admin_id,
        &admin_dev,
        "AC3I Invite Room",
        &AC3I_ROOM_NONCE,
        AC3I_BASE_TS,
    );
    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("validate genesis");
    let genesis_ev_id = genesis_v.event_id;
    store.insert(&genesis_v).expect("insert genesis");

    let cap_hash = capability_hash(&room_id, &AC3I_INVITE_ID, &AC3I_CAP_SECRET);
    let invite_wire = build_member_invited(
        &admin_id,
        &admin_dev,
        &room_id,
        &AC3I_INVITE_ID,
        &cap_hash,
        "member",
        &member_id.identity_key(),
        None,
        None,
        &[genesis_ev_id],
        AC3I_BASE_TS + 1_000,
    );
    let invite_v = validate_wire_bytes(&invite_wire.to_bytes(), &ctx).expect("validate invite");
    store.insert(&invite_v).expect("insert invite");

    room_id.to_string()
}

/// AC3: `room members` text output shows `status=invited` for an invited-but-not-joined member.
#[test]
fn room_members_invited_member_shows_status_invited() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_invited(&home);
    cmd(&home)
        .args(["room", "members", &room_id])
        .assert()
        .success()
        .stdout(predicate::str::contains("status=invited"));
}

/// AC3 + AC4: `room members --json` has `"status":"invited"` for an invited-but-not-joined member.
#[test]
fn room_members_json_invited_member_shows_status_invited() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_invited(&home);
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "room members --json must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    let members = parsed["members"].as_array().expect("members array");
    let invited = members
        .iter()
        .find(|m| m["status"].as_str() == Some("invited"))
        .expect("must find a member with status=invited");
    assert_eq!(
        invited["role"].as_str(),
        Some("member"),
        "invited member must have role=member"
    );
    assert_eq!(
        invited["is_admin"].as_bool(),
        Some(false),
        "invited member must not be admin"
    );
}

/// AC2 + AC3: after genesis + invite (no join), the room has exactly 2 members:
/// admin (active) and the invitee (invited).
#[test]
fn room_members_json_two_members_after_invite() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_invited(&home);
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let members = parsed["members"].as_array().expect("members array");
    assert_eq!(
        members.len(),
        2,
        "after one invite (no join), room must have exactly 2 members (admin + invitee); \
         got: {members:#?}"
    );
}

/// AC3: the invited member's `identity_id` must appear in `room members --json` members array.
#[test]
fn room_members_json_invited_member_identity_id_matches_invitee() {
    let home = TempDir::new().unwrap();
    let room_id = seed_members_invited(&home);
    let out = cmd(&home)
        .args(["room", "members", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let members = parsed["members"].as_array().expect("members array");
    let invitee_id = SigningKey::from_seed(&AC3I_MEMBER_SEED)
        .identity_key()
        .to_string();
    let found = members
        .iter()
        .any(|m| m["identity_id"].as_str() == Some(invitee_id.as_str()));
    assert!(
        found,
        "invitee's identity_id must appear in the members list; members: {members:#?}"
    );
}
