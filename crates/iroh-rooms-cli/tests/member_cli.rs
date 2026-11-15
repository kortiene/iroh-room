//! CI-tier (deterministic, network-free) tests for `room remove` and
//! `room leave` (GA-P0 member removal).
//!
//! Multi-member rooms are built **offline**: real CLI identities and real
//! `room invite` tickets, with each invitee's `member.joined` authored from its
//! own `identity.secret` and replicated into every home's `rooms.db`. That
//! reproduces exactly the persisted state a live `room join` produces, without
//! a network. The live, two-process propagation/denial proofs are the gated
//! tests in `two_peer_e2e.rs`; the Node-level enforcement proofs (connect
//! denial, pipe teardown, blob gate, and "a removed device cannot pull new
//! events from an honest peer") live in `iroh-rooms-net/tests`.
//!
//! Every online step uses `--loopback --timeout 200ms` against peers that are
//! not running, so the "best-effort push" path is exercised and times out
//! deterministically; the event is always persisted locally.

use std::str::FromStr;

use assert_cmd::Command;
use tempfile::TempDir;

use iroh_rooms_core::event::content::Content;
use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::{
    build_member_joined, build_pipe_opened, DeviceBinding, HashRef, IdentityKey, RoomId, SigningKey,
};
use iroh_rooms_core::membership::{
    blob_serve_allowed, pipe_connect_allowed, BlobDecision, DenyReason, MembershipSnapshot,
    PipeDecision, RoomMembership, MAX_ACTIVE_MEMBERS,
};
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::ticket::RoomInviteTicket;

// ── helpers ───────────────────────────────────────────────────────────────────

fn cmd(home: &TempDir) -> Command {
    let mut c = Command::cargo_bin("iroh-rooms").unwrap();
    c.env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(home.path());
    c
}

fn run(home: &TempDir, args: &[&str]) -> std::process::Output {
    cmd(home).args(args).output().expect("run iroh-rooms")
}

fn ok(home: &TempDir, args: &[&str]) -> String {
    let out = run(home, args);
    assert!(
        out.status.success(),
        "`{}` must succeed; stderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output.lines().find_map(|l| {
        l.trim()
            .strip_prefix(key)
            .and_then(|r| r.strip_prefix(':'))
            .map(str::trim)
    })
}

/// A participant: an isolated home with a real CLI identity.
struct Peer {
    home: TempDir,
    id: String,
}

fn peer(name: &str) -> Peer {
    let home = TempDir::new().unwrap();
    ok(&home, &["identity", "create", "--name", name]);
    let json = ok(&home, &["identity", "show", "--json"]);
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let id = v["identity_id"].as_str().unwrap().to_owned();
    Peer { home, id }
}

fn secrets(p: &Peer) -> (SigningKey, SigningKey) {
    let raw = std::fs::read_to_string(p.home.path().join("identity.secret")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let seed = |k: &str| -> [u8; 32] {
        hex::decode(v[k].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap()
    };
    (
        SigningKey::from_seed(&seed("identity_secret")),
        SigningKey::from_seed(&seed("device_secret")),
    )
}

fn store(p: &Peer) -> EventStore {
    EventStore::open(&p.home.path().join("rooms.db")).unwrap()
}

fn event_count(p: &Peer, room: &RoomId) -> usize {
    store(p).room_event_ids(room).unwrap().len()
}

/// Copy every event of `room` from `from` into `to` (idempotent).
fn replicate(from: &Peer, to: &Peer, room: &RoomId) {
    let src = store(from);
    let mut dst = store(to);
    let ctx = ValidationContext::for_room(*room);
    for id in src.room_event_ids(room).unwrap() {
        let wire = src.get(&id).unwrap().unwrap().wire.to_bytes();
        let v = validate_wire_bytes(&wire, &ctx).unwrap();
        dst.insert(&v).unwrap();
    }
}

/// Admin invites `member`, `member` authors its join from its own keys citing the
/// admin's current heads, and the join lands in the admin's store.
fn invite_and_join(admin: &Peer, member: &Peer, room_str: &str, room: &RoomId) {
    let out = ok(
        &admin.home,
        &["room", "invite", room_str, "--invitee", &member.id],
    );
    let token = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("roomtkt1"))
        .expect("ticket");
    let ticket = RoomInviteTicket::from_str(token).unwrap();
    let (id_sk, dev_sk) = secrets(member);
    let heads = store(admin).heads(room).unwrap();
    let binding = DeviceBinding::create(room, &id_sk, dev_sk.device_key());
    let wire = build_member_joined(
        &id_sk,
        &dev_sk,
        room,
        &ticket.invite_id,
        &ticket.capability_secret,
        &ticket.role,
        binding,
        None,
        &heads,
        now_ms(),
    );
    let v = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(*room)).unwrap();
    store(admin).insert(&v).unwrap();
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn status_of(p: &Peer, room: &str, id: &str) -> Option<String> {
    let json = ok(&p.home, &["room", "members", room, "--json"]);
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    v["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["identity_id"].as_str() == Some(id))
        .map(|m| m["status"].as_str().unwrap().to_owned())
}

fn active_count(p: &Peer, room: &str) -> usize {
    let json = ok(&p.home, &["room", "members", room, "--json"]);
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    v["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["status"].as_str() == Some("active"))
        .count()
}

/// Alice (admin) with `n` joined members, replicated to every home.
fn room_with(n: usize) -> (Peer, Vec<Peer>, String, RoomId) {
    let alice = peer("Alice");
    let room_str = field(
        &ok(&alice.home, &["room", "create", "Removal Room"]),
        "room_id",
    )
    .unwrap()
    .to_owned();
    let room = RoomId::from_str(&room_str).unwrap();
    let members: Vec<Peer> = (0..n).map(|i| peer(&format!("M{i}"))).collect();
    for m in &members {
        invite_and_join(&alice, m, &room_str, &room);
    }
    for m in &members {
        replicate(&alice, m, &room);
    }
    (alice, members, room_str, room)
}

const OFFLINE: [&str; 3] = ["--loopback", "--timeout", "200ms"];

fn remove_args<'a>(room: &'a str, who: &'a str) -> Vec<&'a str> {
    let mut a = vec!["room", "remove", room, who];
    a.extend_from_slice(&OFFLINE);
    a
}

fn leave_args(room: &str) -> Vec<&str> {
    let mut a = vec!["room", "leave", room];
    a.extend_from_slice(&OFFLINE);
    a
}

// ── room remove ───────────────────────────────────────────────────────────────

#[test]
fn admin_remove_marks_member_removed_and_prints_honest_note() {
    let (alice, members, room, _) = room_with(2);
    let bob = &members[0];
    assert_eq!(status_of(&alice, &room, &bob.id).as_deref(), Some("active"));

    let out = ok(&alice.home, &remove_args(&room, &bob.id));
    assert_eq!(field(&out, "removed"), Some(bob.id.as_str()));
    assert!(field(&out, "event").is_some_and(|e| e.starts_with("blake3:")));
    assert_eq!(field(&out, "stored"), Some("yes"));
    assert!(
        out.contains("they keep what they already had"),
        "remove must print the honest v0.1 GA note; got:\n{out}"
    );
    assert!(
        out.contains("run `room tail` to sync it to peers"),
        "an undelivered removal must point at room tail; got:\n{out}"
    );
    assert_eq!(
        status_of(&alice, &room, &bob.id).as_deref(),
        Some("removed")
    );
    // The other member is untouched.
    assert_eq!(
        status_of(&alice, &room, &members[1].id).as_deref(),
        Some("active")
    );
}

#[test]
fn removal_survives_restart_and_replicates_to_other_peers() {
    let (alice, members, room_str, room) = room_with(2);
    let (bob, carol) = (&members[0], &members[1]);
    ok(&alice.home, &remove_args(&room_str, &bob.id));

    // A fresh process re-folds the persisted log: the removal is durable.
    assert_eq!(
        status_of(&alice, &room_str, &bob.id).as_deref(),
        Some("removed")
    );
    let tail = ok(
        &alice.home,
        &["room", "tail", &room_str, "--offline", "--json"],
    );
    assert!(
        tail.contains("member.removed"),
        "offline tail must show the persisted removal; got:\n{tail}"
    );

    // Once the event reaches other peers' stores (what a live sync does), every
    // peer folds the same verdict — including the removed member itself.
    replicate(&alice, carol, &room);
    replicate(&alice, bob, &room);
    assert_eq!(
        status_of(carol, &room_str, &bob.id).as_deref(),
        Some("removed")
    );
    assert_eq!(
        status_of(bob, &room_str, &bob.id).as_deref(),
        Some("removed")
    );

    // The removed member can no longer author: its own CLI refuses to send.
    let mut send = vec!["room", "send", room_str.as_str(), "hello?"];
    send.extend_from_slice(&OFFLINE);
    let out = run(&bob.home, &send);
    assert!(
        !out.status.success(),
        "a removed member must not be able to send"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("not_a_member"));
}

#[test]
fn non_admin_remove_is_rejected_with_insufficient_role_and_writes_nothing() {
    let (_alice, members, room_str, room) = room_with(2);
    let (bob, carol) = (&members[0], &members[1]);
    let before = event_count(bob, &room);

    let out = run(&bob.home, &remove_args(&room_str, &carol.id));
    assert_eq!(
        out.status.code(),
        Some(3),
        "insufficient_role is an auth failure (exit 3)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[insufficient_role]"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("only the room admin can remove members"),
        "stderr: {stderr}"
    );
    assert_eq!(
        event_count(bob, &room),
        before,
        "a rejected remove writes nothing"
    );
    assert_eq!(
        status_of(bob, &room_str, &carol.id).as_deref(),
        Some("active")
    );
}

#[test]
fn remove_rejects_self_unknown_already_removed_and_bad_ids() {
    let (alice, members, room_str, room) = room_with(1);
    let bob = &members[0];

    let out = run(&alice.home, &remove_args(&room_str, &alice.id));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot remove yourself"));

    let stranger = peer("Stranger");
    let out = run(&alice.home, &remove_args(&room_str, &stranger.id));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a member"));

    let out = run(&alice.home, &remove_args(&room_str, "not-hex"));
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid member identity id"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("64 hex characters"),
        "the error must describe what the parser accepts; stderr: {stderr}"
    );
    // `IdentityKey` parsing accepts either hex case. Uppercase is not a bad id.
    let upper = "A".repeat(64);
    let out = run(&alice.home, &remove_args(&room_str, &upper));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is not a member"),
        "uppercase hex must parse; stderr: {stderr}"
    );
    assert!(!stderr.contains("invalid member identity id"));

    ok(&alice.home, &remove_args(&room_str, &bob.id));
    let before = event_count(&alice, &room);
    let out = run(&alice.home, &remove_args(&room_str, &bob.id));
    assert_eq!(
        out.status.code(),
        Some(3),
        "already removed is a membership denial (exit 3), same class as not_a_member"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[not_a_member]"), "stderr: {stderr}");
    assert!(
        stderr.contains("already left or been removed"),
        "stderr: {stderr}"
    );
    assert_eq!(event_count(&alice, &room), before);
}

#[test]
fn admin_can_cancel_an_open_invite_by_removing_the_invitee() {
    let alice = peer("Alice");
    let room = field(&ok(&alice.home, &["room", "create", "R"]), "room_id")
        .unwrap()
        .to_owned();
    let bob = peer("Bob");
    ok(
        &alice.home,
        &["room", "invite", &room, "--invitee", &bob.id],
    );
    assert_eq!(
        status_of(&alice, &room, &bob.id).as_deref(),
        Some("invited")
    );
    ok(&alice.home, &remove_args(&room, &bob.id));
    assert_eq!(
        status_of(&alice, &room, &bob.id).as_deref(),
        Some("removed")
    );
}

#[test]
fn removal_frees_a_member_slot_for_a_new_invite() {
    // Alice + (cap - 1) members = the active-member ceiling (5 in the shipped
    // build; 40 under the non-shipped `large_rooms` feature `--all-features` enables).
    let (alice, members, room_str, room) = room_with(MAX_ACTIVE_MEMBERS - 1);
    assert_eq!(active_count(&alice, &room_str), MAX_ACTIVE_MEMBERS);

    // One more join at the ceiling is refused by the fold (stays non-active).
    let frank = peer("Frank");
    invite_and_join(&alice, &frank, &room_str, &room);
    assert_ne!(
        status_of(&alice, &room_str, &frank.id).as_deref(),
        Some("active"),
        "a member beyond the ceiling must not become active"
    );

    // Removing one member frees a slot: a fresh invite + join now succeeds.
    ok(&alice.home, &remove_args(&room_str, &members[0].id));
    assert_eq!(active_count(&alice, &room_str), MAX_ACTIVE_MEMBERS - 1);
    let grace = peer("Grace");
    invite_and_join(&alice, &grace, &room_str, &room);
    assert_eq!(
        status_of(&alice, &room_str, &grace.id).as_deref(),
        Some("active"),
        "the freed slot must admit a new member"
    );
    assert_eq!(active_count(&alice, &room_str), MAX_ACTIVE_MEMBERS);
}

// ── room leave ────────────────────────────────────────────────────────────────

#[test]
fn member_leave_marks_self_removed_everywhere_it_replicates() {
    let (alice, members, room_str, room) = room_with(1);
    let bob = &members[0];

    let out = ok(&bob.home, &leave_args(&room_str));
    assert_eq!(field(&out, "left"), Some(room_str.as_str()));
    assert_eq!(field(&out, "stored"), Some("yes"));
    assert_eq!(status_of(bob, &room_str, &bob.id).as_deref(), Some("left"));

    // Leaving again re-syncs the stored member.left and does not author another.
    let before = event_count(bob, &room);
    let again = ok(&bob.home, &leave_args(&room_str));
    assert_eq!(field(&again, "event"), field(&out, "event"));
    assert_eq!(field(&again, "stored"), Some("yes"));
    assert!(
        again.contains("re-synced the existing member.left"),
        "a repeat leave must say it re-synced; got:\n{again}"
    );
    assert!(
        !again.contains("kept the original"),
        "no new --reason was given, so the reason line stays quiet; got:\n{again}"
    );
    assert_eq!(event_count(bob, &room), before);
    assert_eq!(status_of(bob, &room_str, &bob.id).as_deref(), Some("left"));

    // The admin folds the same verdict once the event reaches its store.
    replicate(bob, &alice, &room);
    assert_eq!(
        status_of(&alice, &room_str, &bob.id).as_deref(),
        Some("left")
    );

    // Removing someone who already left is the same membership denial.
    let out = run(&alice.home, &remove_args(&room_str, &bob.id));
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[not_a_member]"),
        "already left is exit 3; stderr: {stderr}"
    );
    assert!(stderr.contains("already left or been removed"));
}

#[test]
fn repeat_leave_with_a_different_reason_keeps_the_original_event() {
    let (_alice, members, room_str, room) = room_with(1);
    let bob = &members[0];
    let mut first_args = vec![
        "room",
        "leave",
        room_str.as_str(),
        "--reason",
        "first reason",
    ];
    first_args.extend_from_slice(&OFFLINE);
    let first = ok(&bob.home, &first_args);
    let event = field(&first, "event").expect("event id");

    let mut again_args = vec![
        "room",
        "leave",
        room_str.as_str(),
        "--reason",
        "other reason",
    ];
    again_args.extend_from_slice(&OFFLINE);
    let again = ok(&bob.home, &again_args);
    assert_eq!(field(&again, "event"), Some(event));
    assert!(
        again.contains("re-synced the existing member.left"),
        "stdout: {again}"
    );
    assert!(
        again.contains("kept the original"),
        "a different --reason must say the original was kept; stdout: {again}"
    );
    assert_eq!(
        count_member_left(bob, &room),
        1,
        "the retry must not author a second member.left"
    );
}

fn count_member_left(p: &Peer, room: &RoomId) -> usize {
    let store = store(p);
    let ctx = ValidationContext::for_room(*room);
    store
        .room_event_ids(room)
        .unwrap()
        .into_iter()
        .filter(|id| {
            let bytes = store.get(id).unwrap().unwrap().wire.to_bytes();
            let event = validate_wire_bytes(&bytes, &ctx).unwrap();
            matches!(event.event.content, Content::MemberLeft(_))
        })
        .count()
}

#[test]
fn removed_member_cannot_use_leave_to_author_or_resync() {
    let (alice, members, room_str, room) = room_with(1);
    let bob = &members[0];
    ok(&alice.home, &remove_args(&room_str, &bob.id));
    replicate(&alice, bob, &room);
    let before = event_count(bob, &room);

    let out = run(&bob.home, &leave_args(&room_str));
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not_a_member"),
        "an admin removal is not a pending self-leave; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        event_count(bob, &room),
        before,
        "a rejected leave writes nothing"
    );
}

#[test]
fn admin_cannot_leave() {
    let (alice, _members, room_str, room) = room_with(1);
    let before = event_count(&alice, &room);
    let out = run(&alice.home, &leave_args(&room_str));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("the room admin cannot leave"));
    assert_eq!(event_count(&alice, &room), before);
}

#[test]
fn remove_and_leave_require_an_identity_and_a_known_room() {
    let empty = TempDir::new().unwrap();
    let room = "blake3:".to_owned() + &"00".repeat(32);
    let out = run(&empty, &leave_args(&room));
    assert!(!out.status.success());

    let alice = peer("Alice");
    let out = run(&alice.home, &leave_args(&room));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("room_not_found"));
}

#[test]
fn remove_output_never_contains_secret_seeds() {
    let (alice, members, room_str, _) = room_with(1);
    let raw = std::fs::read_to_string(alice.home.path().join("identity.secret")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let out = run(&alice.home, &remove_args(&room_str, &members[0].id));
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for k in ["identity_secret", "device_secret"] {
        assert!(!all.contains(v[k].as_str().unwrap()), "{k} leaked");
    }
}

/// Fold every persisted event of `room` in `p`'s store.
fn fold(p: &Peer, room: &RoomId) -> MembershipSnapshot {
    let st = store(p);
    let ctx = ValidationContext::for_room(*room);
    let events: Vec<_> = st
        .room_event_ids(room)
        .unwrap()
        .into_iter()
        .map(|id| {
            validate_wire_bytes(&st.get(&id).unwrap().unwrap().wire.to_bytes(), &ctx).unwrap()
        })
        .collect();
    RoomMembership::from_events(*room, events).snapshot()
}

#[test]
fn cli_removal_denies_the_removed_device_blobs_and_pipes() {
    let (alice, members, room_str, room) = room_with(1);
    let bob = &members[0];
    let (_bob_id_sk, bob_dev_sk) = secrets(bob);
    let bob_dev = bob_dev_sk.device_key();
    let (alice_id_sk, alice_dev_sk) = secrets(&alice);
    let alice_identity: IdentityKey = alice_id_sk.identity_key();
    let bob_identity: IdentityKey = bob.id.parse().unwrap();

    let hash = HashRef::from_bytes([0xab; 32]);
    let shared_by_alice = move |_h: &HashRef| -> Option<IdentityKey> { Some(alice_identity) };
    let heads = store(&alice).heads(&room).unwrap();
    let pipe_wire = build_pipe_opened(
        &alice_id_sk,
        &alice_dev_sk,
        &room,
        [0x55; 16],
        &alice_dev_sk.device_key(),
        "dev-server",
        "127.0.0.1:8080",
        "iroh-rooms/pipe/1",
        &[bob_identity],
        None,
        &heads,
        now_ms(),
    );
    let pipe_v =
        validate_wire_bytes(&pipe_wire.to_bytes(), &ValidationContext::for_room(room)).unwrap();
    let Content::PipeOpened(pipe) = pipe_v.event.content else {
        panic!("expected pipe.opened");
    };

    // Before removal: the member's device is served blobs and admitted to pipes.
    let snap = fold(&alice, &room);
    assert_eq!(
        blob_serve_allowed(&snap, &bob_dev, &hash, &shared_by_alice),
        BlobDecision::Serve
    );
    assert_eq!(
        pipe_connect_allowed(&snap, &bob_dev, &pipe, None),
        PipeDecision::Accept
    );

    // After the CLI-authored removal: both gates refuse the device (NotActive).
    ok(&alice.home, &remove_args(&room_str, &bob.id));
    let snap = fold(&alice, &room);
    assert_eq!(
        blob_serve_allowed(&snap, &bob_dev, &hash, &shared_by_alice),
        BlobDecision::Reject(DenyReason::NotActive)
    );
    assert_eq!(
        pipe_connect_allowed(&snap, &bob_dev, &pipe, None),
        PipeDecision::Reject(DenyReason::NotActive)
    );
}
