//! Versioned compatibility fixtures for Production Beta.
//!
//! These tests intentionally consume bytes from `tests/fixtures/v1/` instead of
//! rebuilding every event from current code. The regenerated source below is only
//! a maintenance oracle: the fixture file is the compatibility artifact.

#![cfg(feature = "store")]

use std::collections::BTreeMap;

use iroh_rooms_core::event::{
    build_agent_status, build_file_shared, build_member_invited, build_member_joined,
    build_message_text, build_pipe_closed, build_pipe_opened, build_room_created, capability_hash,
    validate_wire_bytes, DeviceBinding, EventId, EventType, HashRef, IdentityKey, RoomId,
    SignedEvent, SigningKey, ValidatedEvent, ValidationContext, WireEvent,
};
use iroh_rooms_core::membership::{Role, RoomMembership, Status};
use iroh_rooms_core::store::EventStore;
use rusqlite::{params, Connection};

const EVENTS_FIXTURE: &str = include_str!("fixtures/v1/events.txt");
const V1_STORE_SCHEMA: &str = include_str!("fixtures/v1/store_v1_schema.sql");

const T0: u64 = 1_750_000_000_000;
const ROOM_NONCE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];
const BOB_INVITE_ID: [u8; 16] = [0xb0; 16];
const BOB_SECRET: [u8; 16] = [
    0x5e, 0xc0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0, 0xb0,
];
const AGENT_INVITE_ID: [u8; 16] = [0xa6; 16];
const AGENT_SECRET: [u8; 16] = [
    0x5e, 0xc0, 0xa6, 0xe7, 0x5e, 0xc0, 0xa6, 0xe7, 0x5e, 0xc0, 0xa6, 0xe7, 0x5e, 0xc0, 0xa6, 0xe7,
];
const FILE_ID: [u8; 16] = [
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
const BLOB_HASH: [u8; 32] = [
    0xdd, 0x10, 0x1e, 0x8f, 0x6f, 0xcf, 0x00, 0x5b, 0x1d, 0xd4, 0x78, 0x0c, 0x4f, 0x7b, 0x73, 0x6c,
    0x4f, 0x45, 0x6c, 0xe2, 0x92, 0xe5, 0x0a, 0x89, 0x6d, 0x1f, 0x40, 0xdf, 0x6d, 0xbe, 0xf3, 0x13,
];
const PIPE_ID: [u8; 16] = [0x9a; 16];
const STATUS_ARTIFACT_ID: [u8; 16] = [0x51; 16];

#[derive(Debug, Clone)]
struct RawFixtureRecord {
    line: usize,
    label: String,
    event_type: String,
    event_id: String,
    wire: Vec<u8>,
}

#[derive(Debug, Clone)]
struct FixtureRecord {
    label: String,
    event_type: String,
    event_id: String,
    wire: Vec<u8>,
    validated: ValidatedEvent,
}

#[derive(Debug, Clone)]
struct GeneratedFixtureRecord {
    label: &'static str,
    event_type: &'static str,
    event: ValidatedEvent,
}

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32])
}

fn alice_id() -> IdentityKey {
    sk(0x01).identity_key()
}

fn bob_id() -> IdentityKey {
    sk(0x03).identity_key()
}

fn agent_id() -> IdentityKey {
    sk(0x05).identity_key()
}

fn room_id() -> RoomId {
    let alice = sk(0x01);
    iroh_rooms_core::event::signed::derive_room_id(&alice.identity_key(), &ROOM_NONCE, T0)
}

fn validate_fixture_wire(wire: &WireEvent) -> ValidatedEvent {
    validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id()))
        .expect("generated fixture event must validate")
}

fn fixture_limit(len: usize) -> u32 {
    u32::try_from(len).expect("fixture record count must fit in u32")
}

#[allow(clippy::too_many_lines)]
fn generated_v1_fixture_source() -> Vec<GeneratedFixtureRecord> {
    let alice_identity = sk(0x01);
    let alice_device = sk(0x02);
    let bob_identity = sk(0x03);
    let bob_device = sk(0x04);
    let agent_identity = sk(0x05);
    let agent_device = sk(0x06);
    let room = room_id();

    let e_create = validate_fixture_wire(&build_room_created(
        &alice_identity,
        &alice_device,
        "Compatibility Room",
        &ROOM_NONCE,
        T0,
    ));

    let bob_cap = capability_hash(&room, &BOB_INVITE_ID, &BOB_SECRET);
    let e_invite_bob = validate_fixture_wire(&build_member_invited(
        &alice_identity,
        &alice_device,
        &room,
        &BOB_INVITE_ID,
        &bob_cap,
        "member",
        &bob_identity.identity_key(),
        None,
        Some("Bob"),
        &[e_create.event_id],
        T0 + 1_000,
    ));

    let bob_binding = DeviceBinding::create(&room, &bob_identity, bob_device.device_key());
    let e_join_bob = validate_fixture_wire(&build_member_joined(
        &bob_identity,
        &bob_device,
        &room,
        &BOB_INVITE_ID,
        &BOB_SECRET,
        "member",
        bob_binding,
        Some("Bob"),
        &[e_invite_bob.event_id],
        T0 + 2_000,
    ));

    let e_message = validate_fixture_wire(&build_message_text(
        &bob_identity,
        &bob_device,
        &room,
        "Compatibility fixture message",
        None,
        None,
        &[],
        &[e_join_bob.event_id],
        T0 + 3_000,
    ));

    let providers = [bob_device.device_key()];
    let e_file = validate_fixture_wire(&build_file_shared(
        &bob_identity,
        &bob_device,
        &room,
        FILE_ID,
        "release-notes.md",
        "text/markdown",
        4_096,
        HashRef::from_bytes(BLOB_HASH),
        Some("raw"),
        &providers,
        &[e_message.event_id],
        T0 + 4_000,
    ));

    let allowed_members = [alice_identity.identity_key(), bob_identity.identity_key()];
    let e_pipe_opened = validate_fixture_wire(&build_pipe_opened(
        &bob_identity,
        &bob_device,
        &room,
        PIPE_ID,
        &bob_device.device_key(),
        "dev-server",
        "localhost:3000",
        "/iroh-rooms/pipe/1",
        &allowed_members,
        None,
        &[e_file.event_id],
        T0 + 5_000,
    ));

    let e_pipe_closed = validate_fixture_wire(&build_pipe_closed(
        &bob_identity,
        &bob_device,
        &room,
        PIPE_ID,
        Some("closed"),
        &[e_pipe_opened.event_id],
        T0 + 6_000,
    ));

    let agent_cap = capability_hash(&room, &AGENT_INVITE_ID, &AGENT_SECRET);
    let e_invite_agent = validate_fixture_wire(&build_member_invited(
        &alice_identity,
        &alice_device,
        &room,
        &AGENT_INVITE_ID,
        &agent_cap,
        "agent",
        &agent_identity.identity_key(),
        None,
        Some("Agent"),
        &[e_pipe_closed.event_id],
        T0 + 7_000,
    ));

    let agent_binding = DeviceBinding::create(&room, &agent_identity, agent_device.device_key());
    let e_join_agent = validate_fixture_wire(&build_member_joined(
        &agent_identity,
        &agent_device,
        &room,
        &AGENT_INVITE_ID,
        &AGENT_SECRET,
        "agent",
        agent_binding,
        Some("Release Agent"),
        &[e_invite_agent.event_id],
        T0 + 8_000,
    ));

    let artifacts = [STATUS_ARTIFACT_ID];
    let e_agent_status = validate_fixture_wire(&build_agent_status(
        &agent_identity,
        &agent_device,
        &room,
        "running_tests",
        Some("P0.7 compatibility fixture verified"),
        &artifacts,
        Some(90),
        &[e_join_agent.event_id],
        T0 + 9_000,
    ));

    vec![
        GeneratedFixtureRecord {
            label: "E_CREATE",
            event_type: "room.created",
            event: e_create,
        },
        GeneratedFixtureRecord {
            label: "E_INVITE_BOB",
            event_type: "member.invited",
            event: e_invite_bob,
        },
        GeneratedFixtureRecord {
            label: "E_JOIN_BOB",
            event_type: "member.joined",
            event: e_join_bob,
        },
        GeneratedFixtureRecord {
            label: "E_MESSAGE",
            event_type: "message.text",
            event: e_message,
        },
        GeneratedFixtureRecord {
            label: "E_FILE",
            event_type: "file.shared",
            event: e_file,
        },
        GeneratedFixtureRecord {
            label: "E_PIPE_OPENED",
            event_type: "pipe.opened",
            event: e_pipe_opened,
        },
        GeneratedFixtureRecord {
            label: "E_PIPE_CLOSED",
            event_type: "pipe.closed",
            event: e_pipe_closed,
        },
        GeneratedFixtureRecord {
            label: "E_INVITE_AGENT",
            event_type: "member.invited",
            event: e_invite_agent,
        },
        GeneratedFixtureRecord {
            label: "E_JOIN_AGENT",
            event_type: "member.joined",
            event: e_join_agent,
        },
        GeneratedFixtureRecord {
            label: "E_AGENT_STATUS",
            event_type: "agent.status",
            event: e_agent_status,
        },
    ]
}

fn parse_raw_event_fixture() -> Vec<RawFixtureRecord> {
    let mut records = Vec::new();
    for (line_idx, line) in EVENTS_FIXTURE.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let columns: Vec<_> = trimmed.split('|').collect();
        assert_eq!(
            columns.len(),
            4,
            "fixtures/v1/events.txt:{} must use label|event_type|event_id|wire_hex",
            line_idx + 1
        );
        let wire = hex::decode(columns[3]).unwrap_or_else(|err| {
            panic!(
                "fixtures/v1/events.txt:{} has invalid wire hex: {err}",
                line_idx + 1
            )
        });
        records.push(RawFixtureRecord {
            line: line_idx + 1,
            label: columns[0].to_owned(),
            event_type: columns[1].to_owned(),
            event_id: columns[2].to_owned(),
            wire,
        });
    }
    assert!(
        !records.is_empty(),
        "fixtures/v1/events.txt must contain at least one event"
    );
    records
}

fn decode_fixture_records() -> Vec<FixtureRecord> {
    parse_raw_event_fixture()
        .into_iter()
        .map(|raw| {
            let wire = WireEvent::decode(&raw.wire).unwrap_or_else(|err| {
                panic!(
                    "fixtures/v1/events.txt:{} ({}) does not decode as WireEvent: {err:?}",
                    raw.line, raw.label
                )
            });
            let event = SignedEvent::decode(&wire.signed).unwrap_or_else(|err| {
                panic!(
                    "fixtures/v1/events.txt:{} ({}) signed bytes do not decode: {err:?}",
                    raw.line, raw.label
                )
            });
            let validated = validate_wire_bytes(&raw.wire, &ValidationContext::for_room(room_id()))
                .unwrap_or_else(|err| {
                    panic!(
                        "fixtures/v1/events.txt:{} ({}) fails stateless validation: {err:?}",
                        raw.line, raw.label
                    )
                });
            assert_eq!(
                event.room_id,
                room_id(),
                "fixtures/v1/events.txt:{} ({}) must belong to the v1 fixture room",
                raw.line,
                raw.label
            );
            assert_eq!(
                validated.wire.to_bytes(),
                raw.wire,
                "fixtures/v1/events.txt:{} ({}) must preserve wire bytes exactly",
                raw.line,
                raw.label
            );
            assert_eq!(
                validated.event_id.to_named_string(),
                raw.event_id,
                "fixtures/v1/events.txt:{} ({}) event_id must match BLAKE3(wire.signed)",
                raw.line,
                raw.label
            );
            assert_eq!(
                validated.event.event_type.as_str(),
                raw.event_type,
                "fixtures/v1/events.txt:{} ({}) event_type must match decoded event",
                raw.line,
                raw.label
            );
            FixtureRecord {
                label: raw.label,
                event_type: raw.event_type,
                event_id: raw.event_id,
                wire: raw.wire,
                validated,
            }
        })
        .collect()
}

fn validated_events(records: &[FixtureRecord]) -> Vec<ValidatedEvent> {
    records
        .iter()
        .map(|record| record.validated.clone())
        .collect()
}

#[test]
fn v1_wire_fixture_decodes_validates_and_folds_current_snapshot() {
    let records = decode_fixture_records();
    let labels_and_types: Vec<_> = records
        .iter()
        .map(|record| (record.label.as_str(), record.event_type.as_str()))
        .collect();
    assert_eq!(
        labels_and_types,
        [
            ("E_CREATE", "room.created"),
            ("E_INVITE_BOB", "member.invited"),
            ("E_JOIN_BOB", "member.joined"),
            ("E_MESSAGE", "message.text"),
            ("E_FILE", "file.shared"),
            ("E_PIPE_OPENED", "pipe.opened"),
            ("E_PIPE_CLOSED", "pipe.closed"),
            ("E_INVITE_AGENT", "member.invited"),
            ("E_JOIN_AGENT", "member.joined"),
            ("E_AGENT_STATUS", "agent.status"),
        ]
    );

    let membership = RoomMembership::from_events(room_id(), validated_events(&records));
    let snapshot = membership.snapshot();
    assert_eq!(snapshot.status(&alice_id()), Some(Status::Active));
    assert_eq!(snapshot.role(&alice_id()), Some(Role::Admin));
    assert_eq!(snapshot.status(&bob_id()), Some(Status::Active));
    assert_eq!(snapshot.role(&bob_id()), Some(Role::Member));
    assert_eq!(snapshot.status(&agent_id()), Some(Status::Active));
    assert_eq!(snapshot.role(&agent_id()), Some(Role::Agent));
    assert_eq!(snapshot.active_members().count(), 3);
}

#[test]
fn v1_wire_fixture_imports_into_current_store_byte_for_byte() {
    let records = decode_fixture_records();
    let mut store = EventStore::open_in_memory().expect("open in-memory store");
    let stats = store
        .insert_all(&validated_events(&records))
        .expect("insert v1 fixture");
    assert_eq!(stats.inserted, records.len() as u64);
    assert_eq!(stats.duplicate, 0);
    assert_eq!(
        store.count(&room_id()).expect("count room"),
        records.len() as u64
    );

    for record in &records {
        let stored = store
            .get(&record.validated.event_id)
            .expect("store get")
            .unwrap_or_else(|| panic!("missing stored event {}", record.label));
        assert_eq!(stored.event_id.to_named_string(), record.event_id);
        assert_eq!(stored.event_type.as_str(), record.event_type);
        assert_eq!(stored.wire.to_bytes(), record.wire);
    }

    for (ty, expected) in [
        (EventType::RoomCreated, 1),
        (EventType::MemberInvited, 2),
        (EventType::MemberJoined, 2),
        (EventType::MessageText, 1),
        (EventType::FileShared, 1),
        (EventType::PipeOpened, 1),
        (EventType::PipeClosed, 1),
        (EventType::AgentStatus, 1),
    ] {
        assert_eq!(
            store
                .by_type(&room_id(), ty)
                .unwrap_or_else(|err| panic!("by_type({}) failed: {err}", ty.as_str()))
                .len(),
            expected,
            "{} count must remain compatible",
            ty.as_str()
        );
    }

    let tail = store
        .room_tail(&room_id(), fixture_limit(records.len()))
        .expect("room tail");
    assert_eq!(tail.len(), records.len());
}

#[test]
fn v1_sqlite_fixture_migrates_to_current_schema_and_rebuilds() {
    let records = decode_fixture_records();
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("iroh-rooms-v1-fixture.sqlite");

    {
        let conn = Connection::open(&db_path).expect("open v1 db");
        conn.execute_batch(V1_STORE_SCHEMA)
            .expect("apply v1 schema");
        insert_v1_fixture_rows(&conn, &records);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read v1 user_version");
        assert_eq!(version, 1);
        assert_eq!(
            count_v2_tables(&conn),
            0,
            "v1 fixture must start without v2 sync-cache tables"
        );
    }

    {
        let mut store = EventStore::open(&db_path).expect("open and migrate v1 fixture");
        assert_eq!(
            store.count(&room_id()).expect("count migrated room"),
            records.len() as u64
        );
        assert_eq!(
            store
                .room_tail(&room_id(), fixture_limit(records.len()))
                .expect("migrated room tail")
                .len(),
            records.len()
        );
        store.rebuild().expect("rebuild migrated fixture");
        assert_eq!(
            store
                .room_tail(&room_id(), fixture_limit(records.len()))
                .expect("rebuilt room tail")
                .len(),
            records.len()
        );
    }

    let conn = Connection::open(&db_path).expect("reopen migrated db");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated user_version");
    assert_eq!(version, 2);
    for table in [
        "sync_state",
        "sync_backfill_tokens",
        "sync_parked",
        "sync_parked_missing",
        "trust_decisions",
    ] {
        assert_table_exists_empty(&conn, table);
    }
    assert_fixture_wires_preserved(&conn, &records);
}

#[test]
fn v1_fixture_file_matches_regenerated_source() {
    let records = decode_fixture_records();
    let generated = generated_v1_fixture_source();
    assert_eq!(records.len(), generated.len());

    for (record, generated) in records.iter().zip(generated) {
        assert_eq!(record.label, generated.label);
        assert_eq!(record.event_type, generated.event_type);
        assert_eq!(
            record.event_id,
            generated.event.event_id.to_named_string(),
            "{} event_id drifted",
            generated.label
        );
        assert_eq!(
            record.wire,
            generated.event.wire.to_bytes(),
            "{} wire bytes drifted",
            generated.label
        );
    }
}

#[test]
#[ignore = "fixture regeneration utility; run explicitly with --ignored --nocapture"]
fn zzz_harvest_v1_fixture() {
    println!("# Iroh Rooms v1 compatibility event fixture");
    println!("# format: label|event_type|event_id|wire_hex");
    println!("# room_id={}", room_id().to_named_string());
    for record in generated_v1_fixture_source() {
        println!(
            "{}|{}|{}|{}",
            record.label,
            record.event_type,
            record.event.event_id.to_named_string(),
            hex::encode(record.event.wire.to_bytes())
        );
    }
}

fn insert_v1_fixture_rows(conn: &Connection, records: &[FixtureRecord]) {
    let mut lamports: BTreeMap<EventId, u64> = BTreeMap::new();
    let mut admin_seqs: BTreeMap<EventId, u64> = BTreeMap::new();
    let admin = alice_id();

    for record in records {
        let event = &record.validated.event;
        let is_genesis = event.event_type == EventType::RoomCreated && event.prev_events.is_empty();
        let lamport = derived_lamport(is_genesis, &event.prev_events, &lamports);
        let admin_seq = derived_admin_seq(
            is_genesis,
            event.sender_id,
            &event.prev_events,
            &admin_seqs,
            admin,
        );

        conn.execute(
            "INSERT INTO events(
                event_id, wire, room_id, sender_id, device_id,
                event_type, created_at, lamport, admin_seq
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &record.validated.event_id.as_bytes()[..],
                &record.wire,
                &event.room_id.as_bytes()[..],
                &event.sender_id.as_bytes()[..],
                &event.device_id.as_bytes()[..],
                event.event_type.as_str(),
                u64_to_i64(event.created_at),
                lamport.map(u64_to_i64),
                admin_seq.map(u64_to_i64),
            ],
        )
        .expect("insert v1 event row");

        for (ordinal, parent) in event.prev_events.iter().enumerate() {
            conn.execute(
                "INSERT INTO event_parents(child_id, parent_id, ordinal)
                 VALUES (?1, ?2, ?3)",
                params![
                    &record.validated.event_id.as_bytes()[..],
                    &parent.as_bytes()[..],
                    i64::try_from(ordinal).expect("parent ordinal fits i64"),
                ],
            )
            .expect("insert v1 parent row");
        }

        if let Some(value) = lamport {
            lamports.insert(record.validated.event_id, value);
        }
        if let Some(value) = admin_seq {
            admin_seqs.insert(record.validated.event_id, value);
        }
    }
}

fn derived_lamport(
    is_genesis: bool,
    parents: &[EventId],
    lamports: &BTreeMap<EventId, u64>,
) -> Option<u64> {
    if is_genesis {
        return Some(0);
    }
    if parents.is_empty() {
        return None;
    }
    parents
        .iter()
        .map(|parent| lamports.get(parent).copied())
        .try_fold(0_u64, |max_seen, value| value.map(|v| max_seen.max(v)))
        .map(|max_parent| max_parent + 1)
}

fn derived_admin_seq(
    is_genesis: bool,
    sender: IdentityKey,
    parents: &[EventId],
    admin_seqs: &BTreeMap<EventId, u64>,
    admin: IdentityKey,
) -> Option<u64> {
    if sender != admin {
        return None;
    }
    if is_genesis {
        return Some(0);
    }
    parents
        .iter()
        .filter_map(|parent| admin_seqs.get(parent).copied())
        .max()
        .map(|seq| seq + 1)
}

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).expect("fixture value fits SQLite INTEGER")
}

fn count_v2_tables(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table'
           AND name IN (
             'sync_state',
             'sync_backfill_tokens',
             'sync_parked',
             'sync_parked_missing',
             'trust_decisions'
           )",
        [],
        |row| row.get(0),
    )
    .expect("count v2 tables")
}

fn assert_table_exists_empty(conn: &Connection, table: &str) {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |row| row.get(0),
        )
        .expect("query sqlite_master");
    assert_eq!(exists, 1, "{table} must exist after migration");

    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|err| panic!("count {table}: {err}"));
    assert_eq!(count, 0, "{table} must start empty after migration");
}

fn assert_fixture_wires_preserved(conn: &Connection, records: &[FixtureRecord]) {
    for record in records {
        let wire: Vec<u8> = conn
            .query_row(
                "SELECT wire FROM events WHERE event_id = ?1",
                params![&record.validated.event_id.as_bytes()[..]],
                |row| row.get(0),
            )
            .unwrap_or_else(|err| panic!("read migrated wire for {}: {err}", record.label));
        assert_eq!(
            wire, record.wire,
            "{} wire changed during migration",
            record.label
        );
    }
}
