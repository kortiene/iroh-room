//! CLI integration tests for `file share` and `file list` (IR-0202 §5 / §9).
//!
//! Coverage map (spec §9 test plan / acceptance criteria):
//!
//!   pre-IO gates  — empty/control-char --mime, empty/control-char --name:
//!                   each exits nonzero before any IO
//!   no identity   — exits nonzero with actionable message
//!   unknown room  — exits nonzero; writes nothing
//!   missing file  — exits nonzero, message contains "no such file"
//!   directory     — exits nonzero, message contains "is a directory"
//!   over-size cap — exits nonzero, output names the cap (env-var override)
//!   happy path    — offline share: exits 0, all required labeled lines present
//!   hash field    — hash is a 64-char hex string (BLAKE3-256 content address)
//!   `event_id`    — has the `blake3:` prefix
//!   `file_id`     — starts with `file_`
//!   provider line — "you (local)" immediately after share
//!   `--mime`      — override is reflected in share output
//!   `--name`      — override is reflected in share output
//!   file list     — empty room prints "(no shared files)"
//!   file list `--json` — empty room emits `[]`
//!   file list     — after a share: `file_id` / name / hash / provider present
//!   file list `--json` — emits a JSON array with the correct provider token
//!   file list after validation — share's `file_id`/`hash` == list's `file_id`/`blob_hash`
//!                   (IR-0203 AC3; the AC4 invalid-rejection proof lives in
//!                   `iroh-rooms-core`'s `membership_store_e2e.rs`, offline of the CLI)
//!   file list unknown room — exits nonzero
//!   consecutive shares — second share does not deadlock on the blob store lock
//!   secret hygiene — device and identity seeds absent from stdout/stderr
//!   size field    — reports the exact byte count of the imported file
//!   `imported:` line — reflects the exact input path
//!   hash round-trip — hash from `file share` == `blob_hash` from `file list --json`
//!   empty file    — a 0-byte file can be shared (exits 0, size: 0 bytes)
//!   MIME guessing — extension-derived MIME type visible at the CLI level
//!   unix-only: chmod 000 → exits nonzero, message contains "permission denied"
//!
//! `file fetch` failure states (IR-0205 §8 test plan) — the deterministic/offline
//! tier only; the live-transfer splits (`peer_unauthorized`, `hash_mismatch`) are
//! e2e:
//!   pre-node gates — invalid file id / bad --timeout / no identity / unknown room
//!                    / malformed --peer: each fails before any node bring-up
//!   invalid arg    — malformed id / unsupported `blob_format` → `error[invalid_argument]:`
//!                    exit 2 (coded, not the pre-IR-0205 generic exit 1)
//!   non-member     — known room, inactive caller → `error[not_a_member]:` exit 3
//!                    (AC2: unauthorized is distinct from unavailable; pre-node)
//!   unavailable    — self-only provider (empty set after self-skip, the early
//!                    pre-loop bail) → `error[blob_unavailable]:` exit 6 + PRD
//!                    §14 language (AC1/AC4)
//!   unavailable, loop path — a real, non-self, never-online provider named and
//!                    genuinely dialed at an unbound loopback port → the *other*
//!                    `blob_unavailable` call site (post-loop, tally-classified),
//!                    within the bounded `--timeout`, no hang (AC1/AC4)

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

/// Write `bytes` to `<dir>/<name>` and return the absolute path string.
fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write fixture file");
    path.to_string_lossy().into_owned()
}

fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

/// Seed `<home>/rooms.db` with a genesis event for a room whose admin is a
/// *different* identity than the CLI user. The room is then known locally (so
/// `file fetch`'s `fold_room` succeeds), but the caller is not an active member —
/// the offline way to reach `fetch`'s membership pre-check without a second live
/// node. Mirrors `pipe_cli.rs::seed_genesis_only`. Returns the `room_id` string.
fn seed_foreign_room(
    home: &TempDir,
    identity_seed: [u8; 32],
    device_seed: [u8; 32],
    nonce: [u8; 16],
) -> String {
    use iroh_rooms_core::event::build_room_created;
    use iroh_rooms_core::event::keys::SigningKey;
    use iroh_rooms_core::event::signed::SignedEvent;
    use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_core::store::EventStore;

    let id_key = SigningKey::from_seed(&identity_seed);
    let dev_key = SigningKey::from_seed(&device_seed);
    let genesis_wire =
        build_room_created(&id_key, &dev_key, "Other Room", &nonce, 1_750_000_000_000);
    let room_id = SignedEvent::decode(&genesis_wire.signed)
        .expect("genesis decodes")
        .room_id;
    let ctx = ValidationContext::for_room(room_id);
    let genesis_v = validate_wire_bytes(&genesis_wire.to_bytes(), &ctx).expect("genesis valid");
    let mut store = EventStore::open(&home.path().join("rooms.db")).expect("open store to seed");
    store.insert(&genesis_v).expect("insert genesis");
    room_id.to_string()
}

/// A syntactically valid `blake3:<hex>` room id (64 hex chars = 32 bytes) that
/// does not exist in any test home. Using all-zeros is valid and unambiguous.
const FAKE_ROOM_ID: &str =
    "blake3:0000000000000000000000000000000000000000000000000000000000000000";

/// The env-var name that overrides the share size cap (spec OQ-4 test seam).
const MAX_SHARE_BYTES_ENV: &str = "IROH_ROOMS_MAX_SHARE_BYTES";

// ── pre-IO gate: --mime empty → exits nonzero ─────────────────────────────────

#[test]
fn share_empty_mime_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["file", "share", FAKE_ROOM_ID, "/tmp/x", "--mime", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--mime"));
}

// ── pre-IO gate: --mime with control char → exits nonzero ────────────────────

#[test]
fn share_control_char_mime_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "file",
            "share",
            FAKE_ROOM_ID,
            "/tmp/x",
            "--mime",
            "text/\nplain",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("control"));
}

// ── pre-IO gate: --name empty → exits nonzero ────────────────────────────────

#[test]
fn share_empty_name_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["file", "share", FAKE_ROOM_ID, "/tmp/x", "--name", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--name"));
}

// ── pre-IO gate: --name with control char → exits nonzero ────────────────────

#[test]
fn share_control_char_name_exits_nonzero() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "file",
            "share",
            FAKE_ROOM_ID,
            "/tmp/x",
            "--name",
            "file\tname",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("control"));
}

// ── no identity → exits nonzero ───────────────────────────────────────────────

#[test]
fn share_without_identity_exits_nonzero() {
    let home = TempDir::new().unwrap();
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "test.txt", b"hello");
    cmd(&home)
        .args(["file", "share", FAKE_ROOM_ID, &path])
        .assert()
        .failure();
}

// ── relative path → resolved against cwd, imports successfully (AC1/AC4) ──────
//
// Regression: `iroh-blobs` `add_path` requires an absolute path, so a relative
// CLI argument must be resolved before import. Without that, `file share` on a
// `./name` path fails with an opaque `blob_import_error` instead of importing.
// Every other test uses absolute `TempDir` paths, so this is the only guard on
// the relative-path case. Run the command from the fixture's directory and pass
// a bare relative file name.

#[test]
fn share_relative_path_is_resolved_and_imports() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    write_file(tmp.path(), "relative.txt", b"relative content");

    let out = cmd(&home)
        .current_dir(tmp.path())
        .args(["file", "share", &room_id, "./relative.txt"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a relative path must import successfully; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // A content-addressed hash proves the relative path was actually read and hashed.
    let hash = extract_field(&stdout, "hash").expect("'hash:' field");
    let hex_part = hash
        .strip_prefix("blake3:")
        .unwrap_or_else(|| panic!("hash must start with 'blake3:'; got {hash:?}"));
    assert_eq!(
        hex_part.len(),
        64,
        "blake3 hash hex part must be 64 chars; got {hex_part:?}"
    );
    // Local provider status proves the bytes actually reached the durable store.
    assert_eq!(
        extract_field(&stdout, "provider"),
        Some("you (local)"),
        "a successfully imported blob must report local provider status"
    );
}

// ── unknown room (identity exists, room not) → exits nonzero ─────────────────

#[test]
fn share_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "test.txt", b"hello");
    cmd(&home)
        .args(["file", "share", FAKE_ROOM_ID, &path])
        .assert()
        .failure();
}

// ── missing file → exits nonzero, message says "no such file" ────────────────

#[test]
fn share_missing_file_exits_nonzero_with_no_such_file() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let missing = home.path().join("does_not_exist.txt");
    cmd(&home)
        .args(["file", "share", &room_id, &missing.to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no such file"));
}

// ── directory instead of file → exits nonzero, says "is a directory" ─────────

#[test]
fn share_directory_exits_nonzero_with_directory_message() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let dir = TempDir::new().unwrap();
    cmd(&home)
        .args(["file", "share", &room_id, &dir.path().to_string_lossy()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("directory"));
}

// ── over size cap (env-var override) → exits nonzero, names the cap ──────────

#[test]
fn share_over_cap_exits_nonzero_and_names_cap() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    // Write 11 bytes, then cap at 10 via the env-var seam so the test is fast.
    let path = write_file(tmp.path(), "big.bin", &[0xFFu8; 11]);
    cmd(&home)
        .env(MAX_SHARE_BYTES_ENV, "10")
        .args(["file", "share", &room_id, &path])
        .assert()
        .failure()
        .stderr(predicate::str::contains("11 bytes"))
        .stderr(predicate::str::contains("10 bytes"));
}

// ── happy path: offline share exits 0, all required fields present ────────────

#[test]
fn share_offline_exits_0_and_prints_required_fields() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "hello.txt", b"hello world");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "file share must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        extract_field(&stdout, "imported").is_some(),
        "output must contain 'imported:' line; got:\n{stdout}"
    );
    assert!(
        extract_field(&stdout, "file_id").is_some(),
        "output must contain 'file_id:'; got:\n{stdout}"
    );
    assert!(
        extract_field(&stdout, "name").is_some(),
        "output must contain 'name:'; got:\n{stdout}"
    );
    assert!(
        extract_field(&stdout, "mime").is_some(),
        "output must contain 'mime:'; got:\n{stdout}"
    );
    assert!(
        extract_field(&stdout, "size").is_some(),
        "output must contain 'size:'; got:\n{stdout}"
    );
    assert!(
        extract_field(&stdout, "hash").is_some(),
        "output must contain 'hash:'; got:\n{stdout}"
    );
    assert!(
        extract_field(&stdout, "event").is_some(),
        "output must contain 'event:'; got:\n{stdout}"
    );
    assert_eq!(
        extract_field(&stdout, "room"),
        Some(room_id.as_str()),
        "'room:' must match the room id"
    );
    assert_eq!(
        extract_field(&stdout, "provider"),
        Some("you (local)"),
        "provider must be 'you (local)' immediately after share"
    );
}

// ── hash is a 64-char lowercase hex string ────────────────────────────────────

#[test]
fn share_hash_field_is_64_char_hex() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "data.bin", b"\x00\x01\x02\x03");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The hash field is displayed as "blake3:<64-hex>" (a named BLAKE3 reference).
    let hash = extract_field(&stdout, "hash").expect("'hash:' field must be present");
    assert!(
        hash.starts_with("blake3:"),
        "hash must start with 'blake3:'; got {hash:?}"
    );
    let hex_part = hash.strip_prefix("blake3:").unwrap();
    assert_eq!(
        hex_part.len(),
        64,
        "blake3 hash hex part must be 64 chars; got {hex_part:?}"
    );
    assert!(
        hex_part
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "hash hex part must be lowercase hex; got {hex_part:?}"
    );
}

// ── event_id has blake3: prefix ───────────────────────────────────────────────

#[test]
fn share_event_id_has_blake3_prefix() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "ev.txt", b"event id prefix test");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let event_id = extract_field(&stdout, "event").expect("'event:' field must be present");
    assert!(
        event_id.starts_with("blake3:"),
        "event_id must start with 'blake3:'; got {event_id:?}"
    );
}

// ── file_id starts with "file_" ───────────────────────────────────────────────

#[test]
fn share_file_id_has_file_prefix() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "id.bin", b"file id prefix test");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let file_id = extract_field(&stdout, "file_id").expect("'file_id:' field must be present");
    assert!(
        file_id.starts_with("file_"),
        "file_id must start with 'file_'; got {file_id:?}"
    );
    // 16 bytes → 32 hex chars after the prefix.
    let hex = file_id.strip_prefix("file_").unwrap();
    assert_eq!(
        hex.len(),
        32,
        "file_id hex part must be 32 chars; got {hex:?}"
    );
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "file_id hex part must be hex; got {hex:?}"
    );
}

// ── --mime override is reflected in share output ──────────────────────────────

#[test]
fn share_mime_override_appears_in_output() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    // A .bin file without --mime would default to application/octet-stream.
    let path = write_file(tmp.path(), "binary.bin", b"binary content");

    let out = cmd(&home)
        .args([
            "file",
            "share",
            &room_id,
            &path,
            "--mime",
            "application/wasm",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        extract_field(&stdout, "mime"),
        Some("application/wasm"),
        "mime must reflect the --mime override"
    );
}

// ── --name override is reflected in share output ──────────────────────────────

#[test]
fn share_name_override_appears_in_output() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "original_name.txt", b"name override test");

    let out = cmd(&home)
        .args([
            "file",
            "share",
            &room_id,
            &path,
            "--name",
            "Pretty Display Name.txt",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        extract_field(&stdout, "name"),
        Some("Pretty Display Name.txt"),
        "name must reflect the --name override"
    );
}

// ── file list: empty room prints "(no shared files)" ─────────────────────────

#[test]
fn list_empty_room_prints_no_shared_files() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["file", "list", &room_id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "file list on empty room must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no shared files"),
        "empty room must say 'no shared files'; got:\n{stdout}"
    );
}

// ── file list: after a share shows the file with provider "you (local)" ───────

#[test]
fn list_after_share_shows_file_with_local_provider() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "listed.txt", b"content to list");

    // Share the file first.
    cmd(&home)
        .args(["file", "share", &room_id, &path])
        .assert()
        .success();

    // Now list.
    let out = cmd(&home)
        .args(["file", "list", &room_id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "file list after share must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("file_"),
        "file list must show a 'file_…' id; got:\n{stdout}"
    );
    assert!(
        stdout.contains("listed.txt"),
        "file list must show the file name; got:\n{stdout}"
    );
    assert!(
        stdout.contains("you (local)"),
        "file list must show 'you (local)' for a locally-imported file; got:\n{stdout}"
    );
}

// ── file list --json: emits a JSON array with the "local" provider token ──────

#[test]
fn list_json_after_share_contains_correct_structure() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "json_test.txt", b"json content");

    cmd(&home)
        .args(["file", "share", &room_id, &path])
        .assert()
        .success();

    let out = cmd(&home)
        .args(["file", "list", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "file list --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output must be valid JSON");
    let files = arr.as_array().expect("output must be a JSON array");
    assert_eq!(files.len(), 1, "must list exactly one file");
    let f = &files[0];
    assert!(f["file_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("file_")));
    assert_eq!(f["name"].as_str(), Some("json_test.txt"));
    assert_eq!(
        f["provider"].as_str(),
        Some("local"),
        "JSON provider token must be 'local' (not 'you (local)')"
    );
    // blob_hash in JSON is the named "blake3:<64-hex>" form (HashRef Display).
    let hash = f["blob_hash"].as_str().expect("blob_hash must be a string");
    assert!(
        hash.starts_with("blake3:"),
        "blob_hash must start with 'blake3:'; got {hash:?}"
    );
    let hex_len = hash.strip_prefix("blake3:").unwrap().len();
    assert_eq!(
        hex_len, 64,
        "blob_hash hex part must be 64 chars; got {hash:?}"
    );
}

// ── file list: shows the exact validated record after `file share` (IR-0203 /
// issue #28 AC3 ⟺ AC4) ───────────────────────────────────────────────────────
//
// The `file_id`/`hash` reported at share time (the author path, which
// self-validates) must be the *exact same* identifiers `file list` (text and
// `--json`) later reports — proving the listed row is the validated record,
// not merely "a" record. The CLI cannot easily inject an invalid `file.shared`
// (the author path validates before printing), so the invalid-rejection proof
// (AC4) lives in the core store test
// `membership_store_e2e::invalid_file_shared_never_persisted_or_listed`; this
// test proves the positive "valid ⇒ listed" half end-to-end.

#[test]
fn shared_file_appears_in_list_after_validation() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "validated.txt", b"validated content");

    let share_out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(
        share_out.status.success(),
        "file share must succeed; stderr: {}",
        String::from_utf8_lossy(&share_out.stderr)
    );
    let share_stdout = String::from_utf8_lossy(&share_out.stdout);
    let file_id = extract_field(&share_stdout, "file_id")
        .expect("'file_id:' field from file share")
        .to_owned();
    let hash = extract_field(&share_stdout, "hash")
        .expect("'hash:' field from file share")
        .to_owned();

    // Text `file list` shows the same file_id and name.
    let list_out = cmd(&home)
        .args(["file", "list", &room_id])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(
        list_stdout.contains(&file_id),
        "file list must show the exact file_id from share; got:\n{list_stdout}"
    );
    assert!(
        list_stdout.contains("validated.txt"),
        "file list must show the file name; got:\n{list_stdout}"
    );

    // `--json` shows the same file_id and blob_hash, i.e. the identical
    // validated record, not a re-derived one.
    let json_out = cmd(&home)
        .args(["file", "list", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let json_stdout = String::from_utf8_lossy(&json_out.stdout);
    let arr: serde_json::Value =
        serde_json::from_str(json_stdout.trim()).expect("file list --json must emit valid JSON");
    let files = arr.as_array().expect("output must be a JSON array");
    assert_eq!(files.len(), 1, "must list exactly one file");
    assert_eq!(
        files[0]["file_id"].as_str(),
        Some(file_id.as_str()),
        "JSON file_id must match the share output's file_id"
    );
    assert_eq!(
        files[0]["blob_hash"].as_str(),
        Some(hash.as_str()),
        "JSON blob_hash must match the share output's hash"
    );
}

// ── file list: unknown room → exits nonzero ───────────────────────────────────

#[test]
fn list_unknown_room_exits_nonzero() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["file", "list", FAKE_ROOM_ID])
        .assert()
        .failure();
}

// ── consecutive shares both succeed (blob store lock released) ────────────────
//
// Regression guard: the second share must not deadlock on the blob store lock
// that the first share opened (spec §9 — import before event, then close).

#[test]
fn two_consecutive_shares_both_succeed() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path1 = write_file(tmp.path(), "first.txt", b"first share");
    let path2 = write_file(tmp.path(), "second.txt", b"second share");

    let out1 = cmd(&home)
        .args(["file", "share", &room_id, &path1])
        .output()
        .unwrap();
    assert!(
        out1.status.success(),
        "first share must exit 0; stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    let out2 = cmd(&home)
        .args(["file", "share", &room_id, &path2])
        .output()
        .unwrap();
    assert!(
        out2.status.success(),
        "second share must exit 0; stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Two distinct files must produce distinct file_ids and distinct event ids.
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let fid1 = extract_field(&stdout1, "file_id").expect("first 'file_id:'");
    let fid2 = extract_field(&stdout2, "file_id").expect("second 'file_id:'");
    assert_ne!(fid1, fid2, "distinct files must produce distinct file_ids");
    let ev1 = extract_field(&stdout1, "event").expect("first 'event:'");
    let ev2 = extract_field(&stdout2, "event").expect("second 'event:'");
    assert_ne!(ev1, ev2, "distinct shares must produce distinct event ids");
}

// ── isolation: data-dir flag keeps homes separate ────────────────────────────

#[test]
fn share_data_dir_isolates_homes() {
    let home1 = TempDir::new().unwrap();
    let home2 = TempDir::new().unwrap();
    create_identity(&home1);
    let room_id = create_room(&home1);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "isolated.txt", b"isolation test");

    // home2 has its own identity but no knowledge of home1's room.
    cmd_at(home2.path())
        .args(["identity", "create", "--name", "Bob"])
        .assert()
        .success();
    cmd_at(home2.path())
        .args(["file", "share", &room_id, &path])
        .assert()
        .failure();
}

// ── secret hygiene: identity and device seeds absent from share output ────────

#[test]
fn share_does_not_expose_secret_seeds() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "secret_hygiene.txt", b"hygiene check");

    let secret_raw =
        std::fs::read_to_string(home.path().join("identity.secret")).expect("identity.secret");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("parse identity.secret");
    let identity_seed = secret_v["identity_secret"]
        .as_str()
        .expect("identity_secret field")
        .to_owned();
    let device_seed = secret_v["device_secret"]
        .as_str()
        .expect("device_secret field")
        .to_owned();

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains(&identity_seed),
        "stdout must not contain the identity secret seed"
    );
    assert!(
        !stderr.contains(&identity_seed),
        "stderr must not contain the identity secret seed"
    );
    assert!(
        !stdout.contains(&device_seed),
        "stdout must not contain the device secret seed"
    );
    assert!(
        !stderr.contains(&device_seed),
        "stderr must not contain the device secret seed"
    );
}

// ── content-hash verification: share reports the BLAKE3 digest of the content ─
//
// Compute the expected BLAKE3-256 independently and assert the `hash:` field
// matches. This is the spec AC2: "Content hash is computed and persisted."

#[test]
fn share_hash_matches_independent_blake3() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let content = b"known content for hash verification";
    let path = write_file(tmp.path(), "hash_check.txt", content);

    // Compute the expected hash independently via the blake3 crate.
    let expected = *blake3::hash(content).as_bytes();
    // The hash field is displayed as "blake3:<64-hex>" (a HashRef named form).
    let expected_named = format!("blake3:{}", hex::encode(expected));

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let reported = extract_field(&stdout, "hash").expect("'hash:' field must be present");
    assert_eq!(
        reported, expected_named,
        "reported hash must equal an independent BLAKE3-256 over the file bytes (named form)"
    );
}

// ── unix-only: chmod 000 file → "permission denied" ──────────────────────────

#[cfg(unix)]
#[test]
fn share_unreadable_file_reports_permission_denied() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let fpath = tmp.path().join("secret.bin");
    std::fs::write(&fpath, b"hidden bytes").unwrap();
    std::fs::set_permissions(&fpath, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = cmd(&home)
        .args(["file", "share", &room_id, &fpath.to_string_lossy()])
        .output()
        .unwrap();

    // Restore perms so TempDir cleanup can remove the file.
    let _ = std::fs::set_permissions(&fpath, std::fs::Permissions::from_mode(0o600));

    assert!(!out.status.success(), "chmod 000 file must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("permission denied"),
        "error must say 'permission denied'; got:\n{stderr}"
    );
}

// ── size field reports the exact byte count ───────────────────────────────────
//
// The happy-path test confirms `size:` is present; this test confirms the value
// is the actual byte count, satisfying AC1 ("File up to MVP target size can be
// imported") at a measured rather than presence-only level.

#[test]
fn share_size_field_matches_actual_byte_count() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let content = b"exactly-seventeen!"; // 18 bytes
    let path = write_file(tmp.path(), "sized.txt", content);

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let size_field = extract_field(&stdout, "size").expect("'size:' field must be present");
    assert_eq!(
        size_field,
        format!("{} bytes", content.len()),
        "size field must equal the exact byte count"
    );
}

// ── `imported:` line reflects the exact input path ───────────────────────────

#[test]
fn share_imported_line_shows_input_path() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "path_check.txt", b"path check");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let imported_val =
        extract_field(&stdout, "imported").expect("'imported:' field must be present");
    assert_eq!(
        imported_val, path,
        "'imported:' must echo the exact path passed on the command line"
    );
}

// ── hash round-trip: `file share` hash == `file list --json` blob_hash ────────
//
// Crosses the persist-then-read persistence boundary (AC2: "Content hash is
// computed and persisted"): the hash stored during `file share` must be
// identical to the hash retrieved by a subsequent `file list --json`.

#[test]
fn share_hash_survives_round_trip_into_list() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "round_trip.txt", b"round trip content");

    let share_out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(
        share_out.status.success(),
        "share must succeed; stderr: {}",
        String::from_utf8_lossy(&share_out.stderr)
    );
    let share_stdout = String::from_utf8_lossy(&share_out.stdout);
    let share_hash = extract_field(&share_stdout, "hash")
        .expect("'hash:' field from file share")
        .to_owned();

    let list_out = cmd(&home)
        .args(["file", "list", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    let arr: serde_json::Value =
        serde_json::from_str(list_stdout.trim()).expect("file list --json must emit valid JSON");
    let list_hash = arr[0]["blob_hash"]
        .as_str()
        .expect("blob_hash field in list output");
    assert_eq!(
        share_hash, list_hash,
        "hash from `file share` must equal blob_hash from `file list --json`"
    );
}

// ── file list --json: empty room emits [] ────────────────────────────────────
//
// The text-mode empty-room test checks "(no shared files)"; this test verifies
// that the JSON mode takes a different, parse-safe path and emits a valid `[]`.

#[test]
fn list_json_empty_room_emits_empty_array() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    let out = cmd(&home)
        .args(["file", "list", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "file list --json on empty room must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output must be valid JSON");
    assert_eq!(
        arr.as_array().expect("must be a JSON array").len(),
        0,
        "empty room must emit [] in JSON mode; got: {stdout}"
    );
}

// ── empty file (0 bytes) can be shared ───────────────────────────────────────
//
// `classify_path` and the blob unit tests already allow 0-byte files; this
// exercises the full CLI path so a 0-byte import is visible end-to-end.

#[test]
fn share_empty_file_succeeds_with_zero_byte_size() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "empty.bin", b"");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a 0-byte file must be shareable; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let size_field = extract_field(&stdout, "size").expect("'size:' field");
    assert_eq!(
        size_field, "0 bytes",
        "size must report '0 bytes' for an empty file; got {size_field:?}"
    );
}

// ── file list: shows all files after multiple shares ─────────────────────────
//
// `two_consecutive_shares_both_succeed` verifies that two `file share` calls
// succeed; this test crosses the persistence→read boundary: both `file.shared`
// events must appear when `file list --json` is called afterward.

#[test]
fn list_json_after_two_shares_returns_two_entries() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path1 = write_file(tmp.path(), "alpha.txt", b"alpha content");
    let path2 = write_file(tmp.path(), "beta.txt", b"beta content");

    cmd(&home)
        .args(["file", "share", &room_id, &path1])
        .assert()
        .success();
    cmd(&home)
        .args(["file", "share", &room_id, &path2])
        .assert()
        .success();

    let out = cmd(&home)
        .args(["file", "list", &room_id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "file list --json after two shares must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("file list --json must emit valid JSON");
    let files = arr.as_array().expect("output must be a JSON array");
    assert_eq!(
        files.len(),
        2,
        "file list must show both shared files; got:\n{stdout}"
    );

    // Both files must report local provider status (this node holds both blobs).
    for f in files {
        assert_eq!(
            f["provider"].as_str(),
            Some("local"),
            "each file must report 'local' provider; got:\n{stdout}"
        );
    }

    // The two entries must have distinct hashes (different content → different CAS).
    let hash0 = files[0]["blob_hash"].as_str().unwrap();
    let hash1 = files[1]["blob_hash"].as_str().unwrap();
    assert_ne!(
        hash0, hash1,
        "distinct content must produce distinct blob hashes"
    );
}

// ── extension-derived MIME type flows through the CLI ─────────────────────────
//
// `guess_mime` is unit-tested in file.rs; this verifies the mapping reaches the
// CLI output so the full share→print pipeline is exercised with a known extension.

#[test]
fn share_mime_is_guessed_from_extension_at_cli_level() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);
    let tmp = TempDir::new().unwrap();
    let path = write_file(tmp.path(), "document.pdf", b"%PDF-1.4");

    let out = cmd(&home)
        .args(["file", "share", &room_id, &path])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        extract_field(&stdout, "mime"),
        Some("application/pdf"),
        "mime must be guessed as application/pdf for a .pdf file when --mime is omitted"
    );
}

// ── file fetch — pre-node argument & precondition gates (IR-0204 §8) ───────────
//
// These exercise the fast, deterministic failure paths that reject BEFORE any
// network node is brought up: a bad file id / bad --timeout is caught pre-IO, a
// missing identity and an unknown room are caught before the consumer node spawns.
// The live transfer paths (valid fetch / hash mismatch / unavailable / unauthorized)
// belong to the two-peer e2e tier, not here.

/// A syntactically valid `file_<32-hex>` handle that resolves to no real file.
const VALID_FILE_ID: &str = "file_00000000000000000000000000000000";

#[test]
fn fetch_invalid_file_id_exits_nonzero() {
    // `parse_file_id` is the very first thing `file fetch` does — a malformed id
    // fails before identity load or any node bring-up. IR-0205 codes this path as
    // `invalid_argument` (exit 2), so a script sees a pinned `error[<code>]:` line
    // and a Usage-category exit, not the pre-IR-0205 generic `error:`/exit 1.
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["file", "fetch", FAKE_ROOM_ID, "not-a-valid-file-id"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[invalid_argument]:"))
        .stderr(predicate::str::contains("invalid file id"));
}

#[test]
fn fetch_bad_timeout_exits_nonzero_pre_io() {
    // `--timeout` is parsed in the dispatcher before `file::fetch` runs, so a bad
    // value writes nothing and dials nothing.
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "file",
            "fetch",
            FAKE_ROOM_ID,
            VALID_FILE_ID,
            "--timeout",
            "nope",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--timeout"));
}

#[test]
fn fetch_no_identity_exits_nonzero() {
    // With a valid id but no local identity, the fetch fails at secret load —
    // still before the node comes up.
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["file", "fetch", FAKE_ROOM_ID, VALID_FILE_ID])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no identity"));
}

#[test]
fn fetch_unknown_room_exits_nonzero() {
    // A known identity fetching from a room that does not exist folds to an empty
    // log and is rejected before any node bring-up.
    let home = TempDir::new().unwrap();
    create_identity(&home);
    cmd(&home)
        .args(["file", "fetch", FAKE_ROOM_ID, VALID_FILE_ID])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no room"));
}

#[test]
fn fetch_malformed_peer_exits_nonzero_pre_io() {
    // `--peer` values are parsed (message::parse_peers) at the very top of
    // `file::fetch`, right after the file id and before any identity load, store
    // open, or node bring-up. A malformed endpoint id must therefore fail fast —
    // even with no identity present — rather than dialing garbage.
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args([
            "file",
            "fetch",
            FAKE_ROOM_ID,
            VALID_FILE_ID,
            "--peer",
            "not-an-endpoint-id",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --peer"));
}

// ── file fetch: an unsupported blob_format is rejected before any provider dial
// (spec §3.3 / §5.2 step 4 / §8 test plan) ─────────────────────────────────────
//
// `file share` always emits `blob_format = "raw"`, so the only way to reach this
// gate at the CLI level is to seed a `hash_seq` `file.shared` directly into the
// room's event store (the same `validate_wire_bytes -> EventStore` pipeline
// `file share` itself uses — see `iroh-rooms-core/tests/file_shared_hashes.rs`).
// Because the reference is already local, `fetch` finds it before any node
// bring-up would be needed to sync it, so the format gate rejects deterministically
// and offline (`--loopback` only guards the still-unconditional node spawn from
// touching a real network).

#[test]
fn fetch_unsupported_blob_format_exits_nonzero_and_writes_nothing() {
    use iroh_rooms_core::event::build_file_shared;
    use iroh_rooms_core::event::content::EventType;
    use iroh_rooms_core::event::ids::{HashRef, RoomId};
    use iroh_rooms_core::event::keys::SigningKey;
    use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_core::store::EventStore;

    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id_str = create_room(&home);
    let room_id: RoomId = room_id_str
        .parse()
        .expect("room create must print a parseable room id");

    // Reconstruct the caller's identity/device signing keys from the profile the
    // CLI just wrote, so the seeded event is authored by (and attributable to) an
    // already-active member — the same device the room genesis bound.
    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret"))
        .expect("identity.secret must exist after identity create");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("identity.secret must be valid JSON");
    let seed_bytes = |field: &str| -> [u8; 32] {
        let hex_str = secret_v[field].as_str().expect("seed field present");
        <[u8; 32]>::try_from(hex::decode(hex_str).expect("seed is valid hex").as_slice())
            .expect("seed is 32 bytes")
    };
    let identity_key = SigningKey::from_seed(&seed_bytes("identity_secret"));
    let device_key = SigningKey::from_seed(&seed_bytes("device_secret"));

    // Seed a `file.shared` with `blob_format = "hash_seq"`, parented on the room's
    // genesis (the only event in a fresh room).
    let mut store = EventStore::open(&home.path().join("rooms.db")).expect("open rooms.db to seed");
    let genesis_id = store
        .by_type(&room_id, EventType::RoomCreated)
        .expect("read room.created events")
        .first()
        .expect("a fresh room has a genesis event")
        .event_id;

    let file_id = [0x77u8; 16];
    let wire = build_file_shared(
        &identity_key,
        &device_key,
        &room_id,
        file_id,
        "collection.bin",
        "application/octet-stream",
        4096,
        HashRef::from_bytes([0x99u8; 32]),
        Some("hash_seq"),
        &[],
        &[genesis_id],
        1,
    );
    let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
        .expect("a hash_seq file.shared still passes stateless validation (spec §3.3)");
    store
        .insert(&validated)
        .expect("seed the hash_seq file.shared");
    drop(store);

    let file_id_handle = format!("file_{}", hex::encode(file_id));
    // IR-0205 codes the format gate as `invalid_argument` (exit 2, Usage): a build
    // limitation the user's argument cannot fix — distinct from the connectivity
    // `blob_unavailable` and never a generic `error:`/exit 1 (spec §5.6 / OQ-4).
    cmd(&home)
        .args(["file", "fetch", &room_id_str, &file_id_handle, "--loopback"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[invalid_argument]:"))
        .stderr(predicate::str::contains("hash_seq"))
        .stderr(predicate::str::contains("raw only"));

    assert!(
        !home.path().join("downloads").exists(),
        "an unsupported-format rejection must write nothing to the downloads dir"
    );
}

// ── file fetch — the honest terminal failure states (IR-0205 §5.1 / AC1+AC2+AC4)
//
// These prove the three headline states are *distinct*, *coded*, and *script-
// friendly* at the process boundary, using only the deterministic/offline tier:
//
//   * non-member  → `error[not_a_member]:`  exit 3 (Auth)         — pre-node, no IO
//   * self-only   → `error[blob_unavailable]:` exit 6 (Connectivity)
//
// The remaining online splits (a live provider that refuses at connect →
// `peer_unauthorized`; a served-but-corrupt blob → `hash_mismatch`) need two live
// processes and belong to the two-peer e2e tier, not here.

/// AC2 — "unauthorized" is a distinct, coded state, never the connectivity-class
/// "unavailable". A caller who is not an active member of a *known* room is refused
/// by `fetch`'s membership pre-check **before any node is brought up**, so this is
/// fully deterministic and offline. The pinned `error[not_a_member]:` line + exit
/// `3` (Auth) is what a script branches on; nothing is written to disk.
#[test]
fn fetch_non_member_is_refused_with_not_a_member_code() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    // A room whose genesis a *different* identity authored: known locally (the fold
    // succeeds) but the CLI caller never joined, so `snapshot.is_active` is false.
    let room_id = seed_foreign_room(&home, [0xF0; 32], [0xF1; 32], [0xFA; 16]);

    cmd(&home)
        .args(["file", "fetch", &room_id, VALID_FILE_ID])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("error[not_a_member]:"))
        .stderr(predicate::str::contains("only an active member can fetch"))
        // AC3 (script-friendly): the failure is reported on stderr only; stdout
        // stays clean so `2>/dev/null` yields an empty saved-path stream.
        .stdout(predicate::str::is_empty());

    assert!(
        !home.path().join("downloads").exists(),
        "a non-member fetch must write nothing to the downloads dir"
    );
}

/// AC1 + AC4 — with no *other* provider online, `file fetch` reports the honest
/// availability limitation (`error[blob_unavailable]:`, exit 6 Connectivity) in
/// PRD §14 language, not a generic failure. We reach the empty-provider-set branch
/// deterministically and offline: the caller shares a file (so the only asserted
/// provider is the caller's own device) and then fetches it — `resolve_providers`
/// skips self, leaving no reachable provider. `--loopback` keeps the ephemeral node
/// off any real network, and because the reference is already local there is no
/// sync-wait or transfer timeout to burn — the empty set bails immediately.
#[test]
fn fetch_self_only_provider_is_unavailable_with_prd_language() {
    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id = create_room(&home);

    // Share a file so a `file.shared` reference exists locally whose sole provider
    // is this node's own device (the `file share` default provider set).
    let src = write_file(
        home.path(),
        "report.txt",
        b"availability follows the providers",
    );
    let share_out = cmd(&home)
        .args(["file", "share", &room_id, &src])
        .output()
        .unwrap();
    assert!(
        share_out.status.success(),
        "share must succeed to seed a reference"
    );
    let share_stdout = String::from_utf8_lossy(&share_out.stdout);
    let file_id = extract_field(&share_stdout, "file_id")
        .expect("file share must print a file_id")
        .to_owned();

    cmd(&home)
        .args(["file", "fetch", &room_id, &file_id, "--loopback"])
        .assert()
        .code(6)
        .stderr(predicate::str::contains("error[blob_unavailable]:"))
        .stderr(predicate::str::contains("is currently unavailable"))
        // AC4: PRD §14 availability language — no queue, no eventual delivery.
        .stderr(predicate::str::contains("no central inbox"))
        .stderr(predicate::str::contains("no guaranteed offline delivery"))
        // Clear retry guidance (§3.2 point 6 / PRD §16 UX req 1): the message
        // names the concrete next step — a provider running `room tail` — rather
        // than implying an automatic queue or eventual delivery.
        .stderr(predicate::str::contains("room tail"))
        .stderr(predicate::str::contains("retry"))
        // AC3 (script-friendly): stdout stays clean; the coded line is on stderr.
        .stdout(predicate::str::is_empty());

    assert!(
        !home.path().join("downloads").exists(),
        "an unavailable fetch must write nothing to the downloads dir"
    );
}

/// AC1 + AC4, second code path — the *loop-exhausted* `blob_unavailable` arm
/// (`file.rs::fetch`'s post-loop `tally.classify() => FetchFailure::Unavailable`
/// branch), distinct from `fetch_self_only_provider_is_unavailable_with_prd_language`
/// above, which only reaches the *earlier*, separate `providers.is_empty()` bail
/// before the per-provider loop ever runs. Here a real, syntactically valid,
/// non-self provider device is named — so `resolve_providers` returns one address
/// and the loop actually dials it — but nobody listens at the given loopback
/// socket, so the dial genuinely fails and `Node::fetch_file` reports
/// `FetchOutcome::Unavailable` within the bounded `--timeout` (mirrors
/// `iroh-rooms-net/tests/blob_e2e.rs::offline_provider_is_reported_unavailable_within_timeout`
/// at the CLI-process boundary, and the same "real endpoint id, unbound loopback
/// port" technique `error_taxonomy_e2e.rs::join_to_an_unreachable_peer_exits_6_no_admin_reachable`
/// already uses for a deterministic, single-process, never-answered dial). This
/// proves the *other* `blob_unavailable` call site — reached only when at least
/// one provider was actually attempted and unreachable — renders the identical
/// coded state, and never hangs past the timeout.
#[test]
fn fetch_unreachable_named_provider_is_unavailable_within_timeout() {
    use iroh_rooms_core::event::build_file_shared;
    use iroh_rooms_core::event::content::EventType;
    use iroh_rooms_core::event::ids::{HashRef, RoomId};
    use iroh_rooms_core::event::keys::SigningKey;
    use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_core::store::EventStore;

    let home = TempDir::new().unwrap();
    create_identity(&home);
    let room_id_str = create_room(&home);
    let room_id: RoomId = room_id_str
        .parse()
        .expect("room create must print a parseable room id");

    // Reconstruct the caller's own identity/device signing keys (the author of
    // the seeded event, and an already-active member) from the profile the CLI
    // just wrote — same technique as the unsupported-format seeding test above.
    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret"))
        .expect("identity.secret must exist after identity create");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("identity.secret must be valid JSON");
    let seed_bytes = |field: &str| -> [u8; 32] {
        let hex_str = secret_v[field].as_str().expect("seed field present");
        <[u8; 32]>::try_from(hex::decode(hex_str).expect("seed is valid hex").as_slice())
            .expect("seed is 32 bytes")
    };
    let identity_key = SigningKey::from_seed(&seed_bytes("identity_secret"));
    let device_key = SigningKey::from_seed(&seed_bytes("device_secret"));

    // A distinct, never-online device — a real key, not the caller's own, so
    // `resolve_providers` does not filter it out as self.
    let ghost_provider_device = SigningKey::from_seed(&[0xEEu8; 32]).device_key();
    let ghost_endpoint_id = iroh::EndpointId::from_bytes(ghost_provider_device.as_bytes())
        .expect("a device key is a valid iroh endpoint id");

    // Seed a `file.shared` naming only the ghost device as provider, parented on
    // the room's genesis (the only event in a fresh room).
    let mut store = EventStore::open(&home.path().join("rooms.db")).expect("open rooms.db to seed");
    let genesis_id = store
        .by_type(&room_id, EventType::RoomCreated)
        .expect("read room.created events")
        .first()
        .expect("a fresh room has a genesis event")
        .event_id;
    let file_id = [0x88u8; 16];
    let wire = build_file_shared(
        &identity_key,
        &device_key,
        &room_id,
        file_id,
        "ghost.bin",
        "application/octet-stream",
        2048,
        HashRef::from_bytes([0xDDu8; 32]),
        Some("raw"),
        &[ghost_provider_device],
        &[genesis_id],
        1,
    );
    let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
        .expect("a file.shared naming a real-but-offline provider still validates statelessly");
    store.insert(&validated).expect("seed the file.shared");
    drop(store);

    // A real endpoint id at a loopback port nobody listens on — port 1 is
    // privileged/unbindable and never used by this suite's live-node tests, so
    // the dial fails deterministically rather than racing a free-port reuse.
    let dead_peer = format!("{ghost_endpoint_id}@127.0.0.1:1");
    let file_id_handle = format!("file_{}", hex::encode(file_id));

    let fetch = cmd(&home)
        .args([
            "file",
            "fetch",
            &room_id_str,
            &file_id_handle,
            "--peer",
            &dead_peer,
            "--timeout",
            "1s",
            "--loopback",
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&fetch.stderr);
    assert_eq!(
        fetch.status.code(),
        Some(6),
        "an unreachable named provider must exit 6 (Connectivity); stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("error[blob_unavailable]:"),
        "must render the pinned blob_unavailable code; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("is currently unavailable"),
        "must use the same honest-unavailable wording as the empty-provider path; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("no central inbox") && stderr.contains("no guaranteed offline delivery"),
        "AC4: PRD §14 availability language; stderr:\n{stderr}"
    );
    // The differentiator from the self-only-provider test: this diagnostic only
    // appears when the per-provider loop actually attempted a dial (proving the
    // *loop-exhausted* bail_coded arm fired, not the earlier empty-set bail).
    assert!(
        stderr.contains("unreachable"),
        "must carry the per-provider 'unreachable' diagnostic, proving the loop \
         actually attempted the dial; stderr:\n{stderr}"
    );
    assert!(
        String::from_utf8_lossy(&fetch.stdout).is_empty(),
        "AC3: stdout stays clean on failure"
    );
    assert!(
        !home.path().join("downloads").exists(),
        "an unavailable fetch must write nothing to the downloads dir"
    );
}
