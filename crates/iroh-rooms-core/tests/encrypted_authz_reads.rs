//! D9 key-aware authorization reads (#191 step 5, spec `content-key-rotation.md`
//! D9 / AC9): `file_shared_hashes`, `pipe_opened`, and `pipe_is_closed` over
//! encrypted envelopes, fail-closed in every direction.
//!
//! What step 5 must prove:
//!
//! * a **readable** encrypted `file.shared` / `pipe.opened` / `pipe.closed`
//!   feeds its projection exactly like plaintext (authenticated decryption +
//!   the D2b strict parse + owner field rules);
//! * an **unreadable** envelope feeds **nothing**: no blob hash is served, no
//!   pipe is known — a keyless node deterministically sees fewer
//!   capabilities, never different ones (fail-closed, never fail-open);
//! * an **unreadable close** cannot be attributed to a pipe (its `pipe_id` is
//!   inside the ciphertext), so it conservatively closes *every* pipe — a
//!   close that might target this pipe is never missed, and an unreadable
//!   close never reopens a closed pipe (AC9);
//! * a malicious Active key holder's sealed-but-invalid body feeds nothing
//!   even where keys are held (D2b), and a foreign-owner encrypted
//!   `pipe.opened` never surfaces;
//! * the governing `pipe.opened` stays deterministic across plaintext and
//!   readable-encrypted candidates (lowest `(lamport, event_id)`).
//!
//! All engine-level (the read surface under test), sharing the step-3/4 cast.

#![cfg(all(feature = "sync", feature = "store"))]

use std::collections::BTreeSet;

use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::constants::{ENCRYPTED_NONCE_LEN, ENCRYPTED_SUITE_V1, SHORT_ID_LEN};
use iroh_rooms_core::event::content::{
    Content, EncryptedContent, EventType, FileShared, PipeClosed, PipeOpened, RoomCreated,
};
use iroh_rooms_core::event::encrypted::{build_content_encrypted, encrypted_content_aad};
use iroh_rooms_core::event::ids::{HashRef, RoomId};
use iroh_rooms_core::event::keys::{IdentityKey, SigningKey};
use iroh_rooms_core::event::signed::{self, SignedEvent};
use iroh_rooms_core::event::wire::WireEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::sim::SimNet;
use iroh_rooms_core::sync::{PeerId, SyncConfig, SyncEngine};
use iroh_rooms_crypto::RoomKey;

// --- Fixtures: the shared step-3/4 golden cast. ------------------------------

const ROOM_NONCE: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
];
const ROOM_CREATED_AT: u64 = 1_750_000_000_000;
const T: u64 = 1_750_000_006_000;
const KEY_EPOCH: u64 = 7;
const PIPE_ID: [u8; SHORT_ID_LEN] = [0x21; 16];
const OTHER_PIPE_ID: [u8; SHORT_ID_LEN] = [0x22; 16];
const BLOB_HASH: [u8; 32] = [0x32; 32];

const WRITER: PeerId = PeerId::from_bytes([0xA1; 32]);
const READER_WITH_KEY: PeerId = PeerId::from_bytes([0xB2; 32]);
const READER_NO_KEY: PeerId = PeerId::from_bytes([0xC3; 32]);
const REMOTE: PeerId = PeerId::from_bytes([0xEE; 32]);

fn alice_identity_secret() -> SigningKey {
    SigningKey::from_seed(&[0x01; 32])
}

fn alice_device_secret() -> SigningKey {
    SigningKey::from_seed(&[0x02; 32])
}

fn alice_identity() -> IdentityKey {
    alice_identity_secret().identity_key()
}

fn room_a() -> RoomId {
    signed::derive_room_id(&alice_identity(), &ROOM_NONCE, ROOM_CREATED_AT)
}

fn room_key() -> RoomKey {
    RoomKey::from_bytes(core::array::from_fn(|i| u8::try_from(i).expect("i < 32")))
}

/// A nonce unique per sealed payload in this file (AES-GCM nonce reuse under
/// one key is catastrophic; tests keep the writer's uniqueness contract too).
fn nonce(tag: u8) -> [u8; ENCRYPTED_NONCE_LEN] {
    let mut n = [0xb0; ENCRYPTED_NONCE_LEN];
    n[ENCRYPTED_NONCE_LEN - 1] = tag;
    n
}

fn genesis_event() -> SignedEvent {
    SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id: alice_identity(),
        device_id: alice_device_secret().device_key(),
        event_type: EventType::RoomCreated,
        created_at: ROOM_CREATED_AT,
        prev_events: vec![],
        content: Content::RoomCreated(RoomCreated {
            room_name: "D9 Reads".to_owned(),
            room_nonce: ROOM_NONCE,
            admins: vec![alice_identity()],
            device_binding: DeviceBinding::create(
                &room_a(),
                &alice_identity_secret(),
                alice_device_secret().device_key(),
            ),
        }),
    }
}

fn seal(ev: &SignedEvent) -> Vec<u8> {
    let csb = ev.to_csb();
    let sig = signed::sign_csb(&csb, &alice_device_secret());
    WireEvent::seal(csb, sig).to_bytes()
}

fn engine_with(config: SyncConfig) -> SyncEngine {
    let store = EventStore::open_in_memory().expect("in-memory store");
    SyncEngine::open(store, room_a(), config).expect("open engine")
}

fn floor_on() -> SyncConfig {
    SyncConfig {
        encrypted_content_writes: true,
        ..SyncConfig::default()
    }
}

fn pipe_opened_body(pipe_id: [u8; SHORT_ID_LEN], label: &str) -> Content {
    Content::PipeOpened(PipeOpened {
        pipe_id,
        owner_id: alice_identity(),
        owner_endpoint: alice_device_secret().device_key(),
        kind: "tcp".to_owned(),
        label: label.to_owned(),
        target_hint: "localhost:5432".to_owned(),
        alpn: "iroh-rooms/pipe/0".to_owned(),
        allowed_members: vec![alice_identity()],
        expires_at: None,
    })
}

fn pipe_closed_body(pipe_id: [u8; SHORT_ID_LEN]) -> Content {
    Content::PipeClosed(PipeClosed {
        pipe_id,
        reason: None,
    })
}

fn file_shared_body_with(tag: u8) -> Content {
    Content::FileShared(FileShared {
        file_id: [tag; 16],
        name: format!("notes-{tag:02x}.txt"),
        mime_type: "text/plain".to_owned(),
        size_bytes: 12,
        blob_hash: HashRef::from_bytes([tag; 32]),
        blob_format: None,
        providers: None,
    })
}

fn file_shared_body() -> Content {
    file_shared_body_with(BLOB_HASH[0])
}

/// An alice-authored encrypted event sealing `inner` under `key`, parented on
/// `parent` via the real writer.
fn sealed_with_parent(
    inner: &Content,
    key: &RoomKey,
    nonce_tag: u8,
    created_at: u64,
    parent: iroh_rooms_core::event::ids::EventId,
) -> WireEvent {
    build_content_encrypted(
        &alice_identity_secret(),
        &alice_device_secret(),
        &room_a(),
        inner,
        KEY_EPOCH,
        key,
        &nonce(nonce_tag),
        &[parent],
        created_at,
    )
    .expect("seal succeeds")
}

/// [`sealed_with_parent`] on the live genesis.
fn sealed(inner: &Content, key: &RoomKey, nonce_tag: u8, created_at: u64) -> WireEvent {
    sealed_with_parent(
        inner,
        key,
        nonce_tag,
        created_at,
        genesis_event().event_id(),
    )
}

/// Seal arbitrary raw bytes as a hand-built envelope claiming `inner_type` —
/// the malicious-Active-key-holder path the builder refuses to produce.
fn sealed_raw(
    plaintext: &[u8],
    inner_type: EventType,
    key: &RoomKey,
    nonce_tag: u8,
    created_at: u64,
) -> Vec<u8> {
    let sender_id = alice_identity();
    let device_id = alice_device_secret().device_key();
    let prev = vec![genesis_event().event_id()];
    let aad = encrypted_content_aad(
        1,
        &room_a(),
        &sender_id,
        &device_id,
        created_at,
        &prev,
        inner_type,
        KEY_EPOCH,
        ENCRYPTED_SUITE_V1,
    );
    let ciphertext = iroh_rooms_crypto::seal_content(key, &nonce(nonce_tag), plaintext, &aad)
        .expect("raw seal succeeds");
    let event = SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id,
        device_id,
        event_type: EventType::ContentEncrypted,
        created_at,
        prev_events: prev,
        content: Content::Encrypted(EncryptedContent {
            inner_type,
            key_epoch: KEY_EPOCH,
            suite: ENCRYPTED_SUITE_V1,
            nonce: nonce(nonce_tag),
            ciphertext,
        }),
    };
    seal(&event)
}

// --- AC9: blob hashes are served only where the key is held. -----------------

/// A readable encrypted `file.shared` contributes its blob hash exactly like
/// plaintext; the same envelope on a keyless node contributes nothing — the
/// blob is not served, while both digests stay converged (D2b).
#[test]
fn encrypted_file_share_serves_hash_only_where_the_key_is_held() {
    let mut net = SimNet::new(room_a());
    net.add_peer(WRITER, engine_with(floor_on()));
    net.add_peer(READER_WITH_KEY, engine_with(SyncConfig::default()));
    net.add_peer(READER_NO_KEY, engine_with(SyncConfig::default()));
    net.engine_mut(WRITER)
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("writer key");
    net.engine_mut(READER_WITH_KEY)
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("reader key");

    net.engine_mut(WRITER)
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");
    net.engine_mut(WRITER)
        .publish(&sealed(&file_shared_body(), &room_key(), 1, T).to_bytes())
        .expect("encrypted file.shared publishes");

    net.connect_all();
    net.run_to_quiescence();
    net.assert_converged(&[WRITER, READER_WITH_KEY, READER_NO_KEY]);

    for node in [WRITER, READER_WITH_KEY] {
        assert_eq!(
            net.engine(node).file_shared_hashes().expect("hashes"),
            BTreeSet::from([BLOB_HASH]),
            "{node:?} holds the key and must serve the shared blob"
        );
    }
    assert!(
        net.engine(READER_NO_KEY)
            .file_shared_hashes()
            .expect("hashes")
            .is_empty(),
        "a keyless node must serve nothing for an unreadable file.shared (AC9)"
    );
}

// --- AC9: the pipe lifecycle through encrypted envelopes. --------------------

/// A readable encrypted `pipe.opened` is known to the gate lookup; a readable
/// encrypted `pipe.closed` closes it. On a keyless node the opened envelope
/// feeds nothing (the pipe is unknown ⇒ unauthorized) and the unreadable
/// close conservatively reads as closed — fail-closed at every combination.
#[test]
fn encrypted_pipe_lifecycle_feeds_the_gate_key_aware() {
    let mut net = SimNet::new(room_a());
    net.add_peer(WRITER, engine_with(floor_on()));
    net.add_peer(READER_WITH_KEY, engine_with(SyncConfig::default()));
    net.add_peer(READER_NO_KEY, engine_with(SyncConfig::default()));
    net.engine_mut(WRITER)
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("writer key");
    net.engine_mut(READER_WITH_KEY)
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("reader key");

    net.engine_mut(WRITER)
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");
    net.engine_mut(WRITER)
        .publish(&sealed(&pipe_opened_body(PIPE_ID, "db"), &room_key(), 2, T).to_bytes())
        .expect("encrypted pipe.opened publishes");

    net.connect_all();
    net.run_to_quiescence();
    net.assert_converged(&[WRITER, READER_WITH_KEY, READER_NO_KEY]);

    // Phase 1 — open, no close anywhere.
    for node in [WRITER, READER_WITH_KEY] {
        let opened = net
            .engine(node)
            .pipe_opened(&PIPE_ID)
            .expect("lookup")
            .expect("key holders must know the encrypted pipe");
        assert_eq!(opened.label, "db");
        assert!(
            !net.engine(node).pipe_is_closed(&PIPE_ID).expect("closed?"),
            "{node:?}: no close exists yet"
        );
    }
    assert!(
        net.engine(READER_NO_KEY)
            .pipe_opened(&PIPE_ID)
            .expect("lookup")
            .is_none(),
        "a keyless node must not know the pipe (unknown ⇒ unauthorized, AC9)"
    );
    assert!(
        !net.engine(READER_NO_KEY)
            .pipe_is_closed(&PIPE_ID)
            .expect("closed?"),
        "no close-typed envelope exists yet, readable or not"
    );

    // Phase 2 — the writer closes the pipe under the same epoch key.
    net.engine_mut(WRITER)
        .publish(&sealed(&pipe_closed_body(PIPE_ID), &room_key(), 3, T + 1).to_bytes())
        .expect("encrypted pipe.closed publishes");
    net.run_to_quiescence();
    net.assert_converged(&[WRITER, READER_WITH_KEY, READER_NO_KEY]);

    for node in [WRITER, READER_WITH_KEY] {
        assert!(
            net.engine(node).pipe_is_closed(&PIPE_ID).expect("closed?"),
            "{node:?}: a readable encrypted close must be honored (AC9)"
        );
    }
    assert!(
        net.engine(READER_NO_KEY)
            .pipe_is_closed(&PIPE_ID)
            .expect("closed?"),
        "an unreadable close is conservatively still-closed on the keyless node"
    );
}

/// The unattributable-close rule in isolation: an unreadable close-typed
/// envelope closes EVERY pipe (its `pipe_id` is sealed — a close that might
/// target this pipe must never be missed), while the plaintext-known pipe
/// stays *known* (the open is not erased, only unauthorized via the gate).
#[test]
fn unreadable_close_conservatively_closes_every_pipe() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");
    let opened = SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id: alice_identity(),
        device_id: alice_device_secret().device_key(),
        event_type: EventType::PipeOpened,
        created_at: T,
        prev_events: vec![genesis_event().event_id()],
        content: pipe_opened_body(PIPE_ID, "plain"),
    };
    engine
        .publish(&seal(&opened))
        .expect("plaintext pipe.opened publishes (floor off)");
    assert!(!engine.pipe_is_closed(&PIPE_ID).expect("closed?"));

    // A remote close sealed under a key this node does NOT hold.
    let foreign_key = RoomKey::from_bytes([0xEE; 32]);
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_closed_body(OTHER_PIPE_ID), &foreign_key, 4, T + 1).to_bytes(),
    );

    assert!(
        engine.pipe_is_closed(&PIPE_ID).expect("closed?"),
        "an unattributable close must conservatively close this pipe too"
    );
    assert!(
        engine.pipe_is_closed(&OTHER_PIPE_ID).expect("closed?"),
        "…and every other pipe id"
    );
    assert!(
        engine.pipe_opened(&PIPE_ID).expect("lookup").is_some(),
        "the plaintext open stays known — only the close side is conservative"
    );
}

/// AC9 verbatim: an unreadable close never *reopens* a closed pipe — a pipe
/// closed by a readable close stays closed when unreadable close-typed
/// envelopes also exist, in every scan order.
#[test]
fn unreadable_close_never_reopens_a_closed_pipe() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("key held");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    // A readable encrypted close for PIPE_ID (ingested — floor off refuses
    // locally-authored envelopes, remote ingest is ungated).
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_closed_body(PIPE_ID), &room_key(), 5, T).to_bytes(),
    );
    assert!(
        engine.pipe_is_closed(&PIPE_ID).expect("closed?"),
        "a readable encrypted close is honored"
    );

    // An unreadable close arrives too: still closed, never reopened.
    let foreign_key = RoomKey::from_bytes([0xEE; 32]);
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_closed_body(PIPE_ID), &foreign_key, 6, T + 1).to_bytes(),
    );
    assert!(
        engine.pipe_is_closed(&PIPE_ID).expect("closed?"),
        "an unreadable close never reopens a closed pipe (AC9)"
    );
}

// --- D2b at the projections: sealed-but-invalid bodies feed nothing. ---------

/// A malicious Active key holder seals garbage claiming `file.shared` /
/// `pipe.opened`: even a node HOLDING the key feeds neither projection (the
/// strict post-decryption parse refuses), and a foreign-owner encrypted
/// `pipe.opened` dies on the owner field rule.
#[test]
fn sealed_but_invalid_bodies_feed_no_projection_even_with_the_key() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("key held");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    // Garbage bytes claiming file.shared: opens, fails strict parse.
    engine.ingest_frame(
        REMOTE,
        &sealed_raw(&[0xFF], EventType::FileShared, &room_key(), 7, T),
    );
    assert!(
        engine.file_shared_hashes().expect("hashes").is_empty(),
        "a sealed-but-invalid file.shared must serve nothing (D2b)"
    );

    // A foreign-owner pipe.opened: opens, parses, fails the owner rule.
    let mallory = SigningKey::from_seed(&[0x66; 32]).identity_key();
    let foreign_owner = Content::PipeOpened(PipeOpened {
        pipe_id: PIPE_ID,
        owner_id: mallory,
        owner_endpoint: alice_device_secret().device_key(),
        kind: "tcp".to_owned(),
        label: "stolen".to_owned(),
        target_hint: "localhost:5432".to_owned(),
        alpn: "iroh-rooms/pipe/0".to_owned(),
        allowed_members: vec![mallory],
        expires_at: None,
    });
    let plaintext = iroh_rooms_core::event::cbor::encode(&foreign_owner.to_cbor());
    engine.ingest_frame(
        REMOTE,
        &sealed_raw(&plaintext, EventType::PipeOpened, &room_key(), 8, T + 1),
    );
    assert!(
        engine.pipe_opened(&PIPE_ID).expect("lookup").is_none(),
        "a foreign-owner encrypted pipe.opened must never surface (D2b/D9)"
    );
}

// --- Determinism of the governing open across plaintext + encrypted. ---------

/// One ordering sub-case of the determinism test: search `created_at` offsets
/// until the encrypted candidate's `event_id` falls on the requested side of
/// the plaintext one (both parent on genesis ⇒ equal lamport ⇒ `event_id`
/// tie-break), then assert the governing open is the lower id. Running BOTH
/// orderings kills both "always prefer plaintext" and "always prefer
/// encrypted" implementations — neither can pass by fixture luck.
fn assert_governing_open_for_ordering(encrypted_wins: bool) {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("key held");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    let plain = SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id: alice_identity(),
        device_id: alice_device_secret().device_key(),
        event_type: EventType::PipeOpened,
        created_at: T,
        prev_events: vec![genesis_event().event_id()],
        content: pipe_opened_body(PIPE_ID, "plain"),
    };
    let plain_id = plain.event_id();

    // Self-adjusting fixture (the step-3 window-test convention): vary the
    // encrypted event's advisory created_at (and nonce, for seal hygiene)
    // until its event_id lands on the requested side of the tie-break.
    let encrypted = (0u8..=255)
        .map(|offset| {
            sealed(
                &pipe_opened_body(PIPE_ID, "enc"),
                &room_key(),
                offset,
                T + u64::from(offset),
            )
        })
        .find(|wire| (signed::event_id_from_bytes(&wire.signed) < plain_id) == encrypted_wins)
        .expect("some created_at offset must produce the requested id order");

    engine
        .publish(&seal(&plain))
        .expect("plaintext open publishes");
    engine.ingest_frame(REMOTE, &encrypted.to_bytes());

    let expected_label = if encrypted_wins { "enc" } else { "plain" };
    let governing = engine
        .pipe_opened(&PIPE_ID)
        .expect("lookup")
        .expect("the pipe is known");
    assert_eq!(
        governing.label, expected_label,
        "the governing open must be the lowest (lamport, event_id) candidate"
    );
}

/// With a plaintext and a readable-encrypted `pipe.opened` for the same
/// `pipe_id`, the governing event is the lowest `(lamport, event_id)` — the
/// same one every keyed node picks, whichever side of the tie-break the
/// encrypted candidate lands on.
#[test]
fn governing_pipe_open_is_deterministic_when_plaintext_wins() {
    assert_governing_open_for_ordering(false);
}

/// See [`governing_pipe_open_is_deterministic_when_plaintext_wins`].
#[test]
fn governing_pipe_open_is_deterministic_when_encrypted_wins() {
    assert_governing_open_for_ordering(true);
}

/// The governing pick orders by lamport FIRST, `event_id` second: an encrypted
/// candidate one causal step deeper (parented on the plaintext open) must
/// lose to the plaintext open even when its `event_id` is hash-luckily
/// smaller — an event_id-only comparison would wrongly govern by it.
#[test]
fn governing_pick_prefers_lower_lamport_over_lower_event_id() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("key held");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    let plain = SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id: alice_identity(),
        device_id: alice_device_secret().device_key(),
        event_type: EventType::PipeOpened,
        created_at: T,
        prev_events: vec![genesis_event().event_id()],
        content: pipe_opened_body(PIPE_ID, "plain"),
    };
    let plain_id = plain.event_id();

    // Deeper lamport (parent = the plaintext open), smaller event_id
    // (self-adjusting created_at search, as in the tie-break tests).
    let deeper = (0u8..=255)
        .map(|offset| {
            sealed_with_parent(
                &pipe_opened_body(PIPE_ID, "deep-enc"),
                &room_key(),
                offset,
                T + 1 + u64::from(offset),
                plain_id,
            )
        })
        .find(|wire| signed::event_id_from_bytes(&wire.signed) < plain_id)
        .expect("some offset must yield a smaller event_id");

    engine
        .publish(&seal(&plain))
        .expect("plaintext open publishes");
    engine.ingest_frame(REMOTE, &deeper.to_bytes());

    let governing = engine
        .pipe_opened(&PIPE_ID)
        .expect("lookup")
        .expect("the pipe is known");
    assert_eq!(
        governing.label, "plain",
        "the lower LAMPORT must govern even against a smaller event_id"
    );
}

// --- Readable encrypted pipe events must discriminate on pipe_id. ------------

/// A READABLE encrypted close closes exactly its own `pipe_id` (unlike the
/// unattributable-unreadable case), and a readable encrypted open surfaces
/// only for its own id — never as the governing open of some other pipe.
#[test]
fn readable_encrypted_pipe_events_discriminate_on_pipe_id() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("key held");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    // A readable encrypted open for PIPE_ID and a readable encrypted close
    // for OTHER_PIPE_ID.
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_opened_body(PIPE_ID, "db"), &room_key(), 0x20, T).to_bytes(),
    );
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_closed_body(OTHER_PIPE_ID), &room_key(), 0x21, T + 1).to_bytes(),
    );

    assert!(
        engine.pipe_opened(&PIPE_ID).expect("lookup").is_some(),
        "the readable encrypted open is known under its own id"
    );
    assert!(
        engine
            .pipe_opened(&OTHER_PIPE_ID)
            .expect("lookup")
            .is_none(),
        "PIPE_ID's open must never surface as the governing open of another id"
    );
    assert!(
        !engine.pipe_is_closed(&PIPE_ID).expect("closed?"),
        "a READABLE close for another pipe must not close this one"
    );
    assert!(
        engine.pipe_is_closed(&OTHER_PIPE_ID).expect("closed?"),
        "the readable close closes exactly its own pipe_id"
    );
}

// --- Accumulation: encrypted hashes union with plaintext ones. ---------------

/// Two readable encrypted `file.shared` and one plaintext one all contribute:
/// the projection is a union, not a first-match (a scan that stopped at the
/// first readable envelope would silently under-serve).
#[test]
fn encrypted_file_shares_accumulate_with_plaintext() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("key held");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    let plain_share = SignedEvent {
        schema_version: 1,
        room_id: room_a(),
        sender_id: alice_identity(),
        device_id: alice_device_secret().device_key(),
        event_type: EventType::FileShared,
        created_at: T,
        prev_events: vec![genesis_event().event_id()],
        content: file_shared_body_with(0x41),
    };
    engine
        .publish(&seal(&plain_share))
        .expect("plaintext file.shared publishes");
    engine.ingest_frame(
        REMOTE,
        &sealed(&file_shared_body_with(0x42), &room_key(), 0x30, T + 1).to_bytes(),
    );
    engine.ingest_frame(
        REMOTE,
        &sealed(&file_shared_body_with(0x43), &room_key(), 0x31, T + 2).to_bytes(),
    );

    assert_eq!(
        engine.file_shared_hashes().expect("hashes"),
        BTreeSet::from([[0x41; 32], [0x42; 32], [0x43; 32]]),
        "plaintext and every readable encrypted share must union"
    );
}

// --- A poisoned epoch (D5a) fails closed through every projection. -----------

/// After a same-epoch key conflict poisons the epoch, envelopes sealed under
/// it read as `EpochConflicted` and every projection fails closed: no blob
/// hash is served, the pipe is unknown, and a close-typed envelope still
/// triggers the conservative close-everything rule (the `EpochConflicted`
/// reason must not be exempted from it).
#[test]
fn poisoned_epoch_fails_closed_through_all_three_reads() {
    let mut engine = engine_with(SyncConfig::default());
    engine
        .insert_room_key(KEY_EPOCH, &room_key())
        .expect("fresh key");
    engine
        .publish(&seal(&genesis_event()))
        .expect("genesis publishes");

    // Envelopes sealed under the original key, ingested BEFORE the conflict —
    // proving a poison also revokes readability retroactively.
    engine.ingest_frame(
        REMOTE,
        &sealed(&file_shared_body_with(0x42), &room_key(), 0x40, T).to_bytes(),
    );
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_opened_body(PIPE_ID, "db"), &room_key(), 0x41, T + 1).to_bytes(),
    );
    assert!(!engine.file_shared_hashes().expect("hashes").is_empty());
    assert!(engine.pipe_opened(&PIPE_ID).expect("lookup").is_some());

    engine
        .insert_room_key(KEY_EPOCH, &RoomKey::from_bytes([0xBB; 32]))
        .expect_err("the conflicting offer poisons the epoch (D5a)");

    assert!(
        engine.file_shared_hashes().expect("hashes").is_empty(),
        "a poisoned epoch serves no blob hashes"
    );
    assert!(
        engine.pipe_opened(&PIPE_ID).expect("lookup").is_none(),
        "a poisoned epoch surfaces no pipes"
    );
    assert!(
        !engine.pipe_is_closed(&PIPE_ID).expect("closed?"),
        "no close-typed envelope exists yet"
    );

    // A close-typed envelope under the poisoned epoch: EpochConflicted must
    // flow into the conservative close-everything arm, not be skipped.
    engine.ingest_frame(
        REMOTE,
        &sealed(&pipe_closed_body(OTHER_PIPE_ID), &room_key(), 0x42, T + 2).to_bytes(),
    );
    assert!(
        engine.pipe_is_closed(&PIPE_ID).expect("closed?"),
        "an EpochConflicted close conservatively closes every pipe"
    );
}
