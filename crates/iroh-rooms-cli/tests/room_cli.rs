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
