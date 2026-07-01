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
//!   file list unknown room — exits nonzero
//!   consecutive shares — second share does not deadlock on the blob store lock
//!   secret hygiene — device and identity seeds absent from stdout/stderr
//!   size field    — reports the exact byte count of the imported file
//!   `imported:` line — reflects the exact input path
//!   hash round-trip — hash from `file share` == `blob_hash` from `file list --json`
//!   empty file    — a 0-byte file can be shared (exits 0, size: 0 bytes)
//!   MIME guessing — extension-derived MIME type visible at the CLI level
//!   unix-only: chmod 000 → exits nonzero, message contains "permission denied"

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
