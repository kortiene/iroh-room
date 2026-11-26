//! CLI checks for upgrade and rollback of an on-disk home.
//!
//! The published rc.6 event bytes live in
//! `iroh-rooms-core/tests/fixtures/rc6/` and are decoded there. This file drives
//! the current `iroh-rooms` binary against a writable data directory:
//! a new test-time identity appends a room without disturbing the rc.6 room, a
//! WAL store whose `user_version` is newer than this binary is refused without
//! changing `rooms.db`, and an `identity.secret` version mismatch fails loudly.
//!
//! The test-time identity is created here. Nothing signs as the identity that
//! produced the rc.6 fixture.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use iroh_rooms::events::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use iroh_rooms::experimental::store::EventStore;
use iroh_rooms::room::RoomId;
use predicates::prelude::*;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const RC6_EVENTS: &str = include_str!("../../iroh-rooms-core/tests/fixtures/rc6/events.txt");
const RC6_STORE_SCHEMA: &str =
    include_str!("../../iroh-rooms-core/tests/fixtures/rc6/store_rc6_schema.sql");

/// Mirrors `schema::USER_VERSION` in `iroh-rooms-core` (currently 2). The
/// downgrade test compares the opened store to this constant and fails loudly
/// if `USER_VERSION` is bumped without updating it.
const SUPPORTED_USER_VERSION: i64 = 2;

fn cmd(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("iroh-rooms").unwrap();
    command
        .env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(home.path());
    command
}

fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

fn rc6_room_id() -> String {
    RC6_EVENTS
        .lines()
        .find_map(|line| line.trim().strip_prefix("# room_id="))
        .expect("rc.6 fixture must declare a room_id header")
        .to_owned()
}

fn rc6_event_ids() -> Vec<String> {
    RC6_EVENTS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split('|')
                .nth(2)
                .expect("fixture line must carry an event id")
                .to_owned()
        })
        .collect()
}

fn rc6_validated_events(room: RoomId) -> Vec<ValidatedEvent> {
    let mut events = Vec::new();
    for (idx, line) in RC6_EVENTS.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut columns = line.split('|');
        let label = columns.next().unwrap_or("event");
        let event_type = columns.next().expect("event_type column");
        let event_id = columns.next().expect("event_id column");
        let wire_hex = columns.next().expect("wire column");
        assert!(
            columns.next().is_none(),
            "line {} has extra columns",
            idx + 1
        );
        let wire = hex::decode(wire_hex).unwrap_or_else(|err| panic!("{label} wire hex: {err}"));
        let validated = validate_wire_bytes(&wire, &ValidationContext::for_room(room))
            .unwrap_or_else(|err| panic!("{label} failed validation: {err:?}"));
        assert_eq!(validated.event.event_type.as_str(), event_type);
        assert_eq!(validated.event_id.to_string(), event_id);
        assert_eq!(validated.wire.to_bytes(), wire);
        events.push(validated);
    }
    assert!(!events.is_empty(), "rc.6 fixture must contain events");
    events
}

fn read_user_version(db_path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("open rooms.db");
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read user_version")
}

fn seed_rc6_store(home: &Path) {
    let db_path = home.join("rooms.db");
    {
        let conn = rusqlite::Connection::open(&db_path).expect("create rc.6 schema db");
        conn.execute_batch(RC6_STORE_SCHEMA)
            .expect("apply harvested rc.6 schema");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read user_version after schema");
        assert_eq!(
            version, SUPPORTED_USER_VERSION,
            "store_rc6_schema.sql must stamp user_version 2"
        );
    }

    let room: RoomId = rc6_room_id().parse().expect("rc.6 room id parses");
    let events = rc6_validated_events(room);
    // The schema is already v2, so this open is a no-op migration.
    let mut store = EventStore::open(&db_path).expect("open rc.6-shaped store");
    let stats = store
        .insert_all(&events)
        .expect("insert rc.6 fixture events");
    assert_eq!(stats.inserted, events.len() as u64);
    assert_eq!(stats.duplicate, 0);
    drop(store);
    assert_eq!(
        read_user_version(&db_path),
        SUPPORTED_USER_VERSION,
        "opening the rc.6 schema must leave user_version at 2"
    );
}

fn tail_offline(home: &TempDir, room_id: &str) -> String {
    let output = cmd(home)
        .args(["room", "tail", room_id, "--offline"])
        .output()
        .expect("spawn room tail");
    assert!(
        output.status.success(),
        "room tail --offline {room_id} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("tail stdout is utf-8")
}

fn sha256_file(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    hex::encode(Sha256::digest(bytes))
}

fn wal_sidecar(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().expect("db file name").to_os_string();
    name.push("-wal");
    db_path.with_file_name(name)
}

/// Stamp `PRAGMA user_version = USER_VERSION + 1`, checkpoint, and close so the
/// hashed `rooms.db` has no `-wal` sidecar.
fn stamp_future_user_version_and_checkpoint(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).expect("open rooms.db");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal_mode");
    assert_eq!(
        mode.to_ascii_lowercase(),
        "wal",
        "downgrade refusal is exercised on a WAL store"
    );
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read user_version");
    assert_eq!(current, SUPPORTED_USER_VERSION);
    conn.pragma_update(None, "user_version", SUPPORTED_USER_VERSION + 1)
        .expect("stamp a newer user_version");
    let checkpoint: (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("checkpoint the WAL");
    assert_eq!(checkpoint.0, 0, "wal checkpoint must not report busy");
    drop(conn);

    let wal = wal_sidecar(db_path);
    assert!(
        !wal.exists(),
        "checkpoint must remove {} before rooms.db is hashed",
        wal.display()
    );
}

fn bump_identity_secret_version(home: &Path, version: u64) {
    let path = home.join("identity.secret");
    let raw = std::fs::read(&path).expect("read identity.secret");
    let mut value: serde_json::Value =
        serde_json::from_slice(&raw).expect("identity.secret must be JSON");
    value["version"] = serde_json::json!(version);
    std::fs::write(
        &path,
        serde_json::to_vec(&value).expect("encode identity.secret"),
    )
    .expect("rewrite identity.secret");
}

/// A test-time identity creates a second room in the rc.6 store. After that
/// process exits, `room tail --offline` of the original room still matches.
#[test]
fn rc6_old_room_tail_matches_after_new_identity_creates_a_room() {
    let home = TempDir::new().unwrap();
    seed_rc6_store(home.path());
    cmd(&home)
        .args(["identity", "create", "--name", "Upgrade Tester"])
        .assert()
        .success();

    let old_room = rc6_room_id();
    let before = tail_offline(&home, &old_room);
    for event_id in rc6_event_ids() {
        assert!(
            before.contains(&event_id),
            "pre-write tail must contain harvested event {event_id}"
        );
    }

    let created = cmd(&home)
        .args(["room", "create", "Upgrade Side Room"])
        .output()
        .expect("spawn room create");
    assert!(
        created.status.success(),
        "test-time identity must create a new room: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created_stdout = String::from_utf8_lossy(&created.stdout);
    let new_room = extract_field(&created_stdout, "room_id")
        .expect("room create must print room_id")
        .to_owned();
    assert_ne!(new_room, old_room, "the new room is not the rc.6 room");

    let after = tail_offline(&home, &old_room);
    assert_eq!(
        after, before,
        "room tail --offline of the rc.6 room must match after reopen"
    );

    let new_tail = tail_offline(&home, &new_room);
    assert!(
        new_tail.contains("type=room.created"),
        "the new room must have its own genesis event"
    );
    for event_id in rc6_event_ids() {
        assert!(
            !new_tail.contains(&event_id),
            "the new room must not contain harvested event {event_id}"
        );
    }
    assert_eq!(
        read_user_version(&home.path().join("rooms.db")),
        SUPPORTED_USER_VERSION,
        "CLI use of the rc.6 store must leave user_version at 2"
    );
}

/// A checkpointed WAL store with `user_version = USER_VERSION + 1` is refused.
/// `rooms.db` is unchanged. Sidecar files are not asserted.
#[test]
fn wal_store_newer_user_version_is_refused_without_changing_rooms_db() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Downgrade Probe"])
        .assert()
        .success();
    let created = cmd(&home)
        .args(["room", "create", "Downgrade Probe Room"])
        .output()
        .expect("spawn room create");
    assert!(
        created.status.success(),
        "setup room create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let room_id = extract_field(&String::from_utf8_lossy(&created.stdout), "room_id")
        .expect("setup room_id")
        .to_owned();

    let db_path = home.path().join("rooms.db");
    stamp_future_user_version_and_checkpoint(&db_path);
    let before = sha256_file(&db_path);

    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .code(1)
        .stderr(
            predicate::str::is_match(
                "store migration failed: database user_version [0-9]+ is newer than supported [0-9]+",
            )
            .expect("prefix regex compiles"),
        );

    assert_eq!(
        sha256_file(&db_path),
        before,
        "rooms.db sha256 must be unchanged after the refused open"
    );
}

fn header_bytes_18_19(path: &Path) -> [u8; 2] {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert!(
        bytes.len() >= 20,
        "sqlite header truncated in {}",
        path.display()
    );
    [bytes[18], bytes[19]]
}

/// Rollback journal (`journal_mode = DELETE`): header bytes 18-19 are 1/1,
/// then `user_version` is one past what this binary supports.
fn stamp_future_user_version_delete_journal(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).expect("open rooms.db");
    let mode: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .expect("switch to DELETE journal");
    assert_eq!(mode.to_ascii_lowercase(), "delete");
    conn.pragma_update(None, "user_version", SUPPORTED_USER_VERSION + 1)
        .expect("stamp a newer user_version");
    drop(conn);
    assert_eq!(
        header_bytes_18_19(db_path),
        [1, 1],
        "DELETE journal stores write version/read version 1"
    );
}

/// A non-WAL store with `user_version = USER_VERSION + 1` is refused.
/// `rooms.db`'s sha256 and header bytes 18-19 stay unchanged. On current main
/// those bytes move from 1/1 to 2/2 because WAL pragmas run before the check.
#[test]
fn delete_journal_store_newer_user_version_is_refused_without_rewriting_header() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Downgrade Probe"])
        .assert()
        .success();
    let created = cmd(&home)
        .args(["room", "create", "Downgrade Probe Room"])
        .output()
        .expect("spawn room create");
    assert!(
        created.status.success(),
        "setup room create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let room_id = extract_field(&String::from_utf8_lossy(&created.stdout), "room_id")
        .expect("setup room_id")
        .to_owned();

    let db_path = home.path().join("rooms.db");
    stamp_future_user_version_delete_journal(&db_path);
    let before = sha256_file(&db_path);
    let before_header = header_bytes_18_19(&db_path);

    cmd(&home)
        .args(["room", "tail", &room_id, "--offline"])
        .assert()
        .code(1)
        .stderr(
            predicate::str::is_match(
                "store migration failed: database user_version [0-9]+ is newer than supported [0-9]+",
            )
            .expect("prefix regex compiles"),
        );

    assert_eq!(
        sha256_file(&db_path),
        before,
        "rooms.db sha256 must be unchanged after the refused open"
    );
    assert_eq!(
        header_bytes_18_19(&db_path),
        before_header,
        "header bytes 18-19 must stay {before_header:?} after the refused open"
    );
}

/// `identity.secret` `version` other than the version this binary writes fails
/// the next command that loads the secret.
#[test]
fn identity_secret_version_mismatch_fails_loudly() {
    let home = TempDir::new().unwrap();
    cmd(&home)
        .args(["identity", "create", "--name", "Version Probe"])
        .assert()
        .success();
    bump_identity_secret_version(home.path(), 99);

    cmd(&home)
        .args(["room", "create", "Should Not Persist"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "unsupported identity.secret version",
        ));
    assert!(
        !home.path().join("rooms.db").exists(),
        "a rejected identity load must not create rooms.db"
    );
}
