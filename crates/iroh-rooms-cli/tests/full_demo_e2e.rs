//! The full two-humans-plus-one-agent demo integration test (IR-0209 / issue #34).
//!
//! Phase 1A shipped the two-human product slice and its integration test
//! (`two_peer_e2e.rs`, #24). Phase 1B then completed the MVP surface —
//! recent-history sync, the full Blob Plane, agent identity and agent status,
//! pipe security warnings — each with its own unit and end-to-end tests. This
//! suite is the one integration test PRD §19 Phase 1B deliverable 8 names
//! explicitly: it chains the **entire** MVP demo together with the full cast —
//! **two humans and one agent** — driven through the real `iroh-rooms` binary
//! across three isolated on-disk homes, **without a central application
//! server**.
//!
//! This is *not* a re-run of `two_peer_e2e.rs` (no agent) or `agent_e2e.rs` (no
//! second human, no file share, no live pipe): it is the unified three-party
//! flow, plus two assertions strengthened to full cast/type coverage:
//!
//! 1. "All events validate after restart" across the *entire* event-type
//!    diversity the demo produces, not just a single `message.text`.
//! 2. "Agent can post status but has no implicit extra privilege" — the agent
//!    posts a signed status *and* is refused an admin-only action from its own
//!    home, proving membership grants it exactly "active member," nothing more.
//!
//! ## Network stack
//!
//! Every online step runs with the hidden `--loopback` flag, which routes
//! through `NetMode::Loopback` (`presets::Minimal` + `RelayMode::Disabled`,
//! `crates/iroh-rooms-net/src/transport.rs`): no relay server, no n0
//! discovery — pure loopback QUIC over `127.0.0.1`, wired together by parsing
//! each host's `listening:` address and threading it into the peer's `--peer`.
//! That relay-free, discovery-free stack is both what makes the online tier
//! hermetic and the literal proof of AC1 ("completes without central
//! application server"). The harness itself spawns only the three
//! `iroh-rooms` children plus an in-test loopback echo target (the *service
//! being exposed* by the pipe leg, not infrastructure) — no server of any kind.
//!
//! ## Tiers
//!
//! | Issue AC | Test | Tier |
//! |---|---|---|
//! | AC1 — no central server (backbone) | `full_slice_runs_without_central_server` | CI (`#[test]`) |
//! | AC1 — no central server (full flow) | `full_demo_two_humans_one_agent` | gated (`#[ignore]`) |
//! | AC2 — all events validate after restart (full type set) | `all_event_types_validate_after_restart` | CI (`#[test]`) |
//! | AC2 — event *content* survives restart (per-type field audit) | `seeded_event_content_survives_restart` | CI (`#[test]`) |
//! | AC2 — departures fold after restart (left/removed lifecycle) | `seeded_full_chain_membership_fold_reflects_departures` | CI (`#[test]`) |
//! | AC2 — wire-delivered events validate after restart | `full_demo_log_validates_after_restart` | gated (`#[ignore]`) |
//! | AC3 — file content hash verifies | inside `full_demo_two_humans_one_agent` | gated (`#[ignore]`) |
//! | AC4 — pipe access explicitly authorized | `authorized_pipe_forwards_bytes_three_party` | gated (`#[ignore]`) |
//! | AC4 — unauthorized connection denied | `unauthorized_member_pipe_denied` | gated (`#[ignore]`) |
//! | AC5 — agent posts status, no admin privilege | `agent_posts_status_but_has_no_admin_privilege` | CI (`#[test]`) |
//! | three-way convergence (support) | `three_way_membership_converges` | gated (`#[ignore]`) |
//! | harness self-guards | `child_session_captures_output`, `child_session_captures_stderr` | CI (`#[test]`) |
//!
//! The CI tier is deterministic and network-free (offline reads/writes over
//! `rooms.db`, seeded via the pure core event builders) so it can never flake
//! `scripts/verify.sh`. The online tier needs three live loopback processes to
//! rendezvous, so it is `#[ignore]`-gated and run with the documented command
//! below. Every AC also has a green-in-CI lower-layer backstop (the Node-API
//! e2e suites and the two existing CLI online suites, `two_peer_e2e.rs` /
//! `agent_e2e.rs`), so gating this suite's online tier loses no guaranteed
//! coverage — it adds the unified three-party product proof on top.
//!
//! ## Running the gated tier
//!
//! ```bash
//! # Full two-humans-plus-one-agent demo proof (membership convergence, message,
//! # file fetch+verify, live pipe, agent status, restart validation). Loopback
//! # only; no relay, no external tools. Serialize to avoid port/resource
//! # contention across three-process tests.
//! cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1
//! ```
//!
//! No production code is modified: everything driven here (`agent invite`,
//! `room tail --accept-joins --loopback`, `room join --peer --loopback`, `room
//! send`, `agent status`, `file share`/`file fetch`, `pipe expose`/`pipe
//! connect`/`pipe close`, `room members --json`, `room tail --offline --json`)
//! already ships and is reconciled to the binary in `docs/getting-started.md`.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
use iroh_rooms_core::event::{
    build_agent_status, build_file_shared, build_member_invited, build_member_joined,
    build_member_left, build_member_removed, build_message_text, build_pipe_closed,
    build_pipe_opened, build_room_created, capability_hash, signed, DeviceBinding, EventId,
    HashRef, SigningKey, WireEvent,
};
use iroh_rooms_core::store::EventStore;

/// Per-network-step budget (matches the sibling suites). Every wait is bounded
/// by this so a rendezvous bug fails fast instead of hanging CI.
const WAIT: Duration = Duration::from_secs(15);

// ── binary + one-shot helpers (ported from two_peer_e2e.rs / agent_e2e.rs) ────

/// Resolve the built `iroh-rooms` binary once, so both `assert_cmd` one-shots
/// and the raw `std::process::Command` child sessions target the same artifact.
fn bin_path() -> PathBuf {
    assert_cmd::cargo::cargo_bin("iroh-rooms")
}

/// Run a one-shot `iroh-rooms <args…>` against `dir`, capturing its output.
/// Isolation mirrors the other CLI suites: `IROH_ROOMS_HOME` is removed and
/// `--data-dir` pins the home explicitly.
fn one_shot(dir: &Path, args: &[&str]) -> std::process::Output {
    assert_cmd::Command::cargo_bin("iroh-rooms")
        .expect("iroh-rooms binary must build")
        .env_remove("IROH_ROOMS_HOME")
        .arg("--data-dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("run iroh-rooms one-shot")
}

/// The trimmed value that follows `key:` on a `key: value` line.
fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

/// The `roomtkt1…` token printed on the indented line under `ticket:`.
fn extract_ticket(stdout: &str) -> Option<&str> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("ticket:") {
            return lines.next().map(str::trim);
        }
    }
    None
}

/// Parse a `listening: <ENDPOINT_ID>[@<ip:port>,…]` line into the bare `--peer`
/// address (everything after `listening:`).
fn parse_listening(line: &str) -> String {
    extract_field(line, "listening").unwrap_or("").to_owned()
}

/// Parse the bound loopback socket out of a `forwarding: 127.0.0.1:<port> ->
/// pipe <id>` line (the first whitespace token after `forwarding:`).
fn parse_forwarding(line: &str) -> SocketAddr {
    let rest = extract_field(line, "forwarding").unwrap_or("");
    let token = rest.split_whitespace().next().unwrap_or("");
    token
        .parse()
        .unwrap_or_else(|_| panic!("could not parse forwarding socket from {line:?}"))
}

// ── fixture helpers (identity / room / invite / agent invite / roster) ────────

fn identity_create(home: &TempDir, name: &str) {
    let out = one_shot(home.path(), &["identity", "create", "--name", name]);
    assert!(
        out.status.success(),
        "identity create must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The local `identity_id`, parsed from `identity show --json`.
fn identity_id(home: &TempDir) -> String {
    let out = one_shot(home.path(), &["identity", "show", "--json"]);
    assert!(
        out.status.success(),
        "identity show --json must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("identity show --json must be valid JSON");
    value["identity_id"]
        .as_str()
        .expect("identity show --json must carry an identity_id")
        .to_owned()
}

/// `iroh-rooms room create <name>` → the `room_id`.
fn room_create(home: &TempDir, name: &str) -> String {
    let out = one_shot(home.path(), &["room", "create", name]);
    assert!(
        out.status.success(),
        "room create must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_field(&stdout, "room_id")
        .expect("room create must print a room_id")
        .to_owned()
}

/// `iroh-rooms room invite <room> --invitee <id> --role <role> [--expires <e>]`
/// → the `roomtkt1…` ticket.
fn invite(
    home: &TempDir,
    room: &str,
    invitee_id: &str,
    role: &str,
    expires: Option<&str>,
) -> String {
    let mut args = vec![
        "room",
        "invite",
        room,
        "--invitee",
        invitee_id,
        "--role",
        role,
    ];
    if let Some(e) = expires {
        args.push("--expires");
        args.push(e);
    }
    let out = one_shot(home.path(), &args);
    assert!(
        out.status.success(),
        "room invite must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_ticket(&stdout)
        .expect("room invite must print a roomtkt1… ticket")
        .to_owned()
}

/// `iroh-rooms agent invite <room> <agent_id> [--expires <e>]` → the
/// `roomtkt1…` ticket. Exercises the documented `agent` noun (not `room invite
/// --role agent`), mirroring `agent_e2e.rs`.
fn agent_invite(home: &TempDir, room: &str, agent_id: &str) -> String {
    let out = one_shot(home.path(), &["agent", "invite", room, agent_id]);
    assert!(
        out.status.success(),
        "agent invite must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        extract_field(&stdout, "role"),
        Some("agent"),
        "agent invite must report role: agent"
    );
    extract_ticket(&stdout)
        .expect("agent invite must print a roomtkt1… ticket")
        .to_owned()
}

/// The parsed `room members --json` roster object for `room` from `home`.
fn members_json(home: &TempDir, room: &str) -> serde_json::Value {
    let out = one_shot(home.path(), &["room", "members", room, "--json"]);
    assert!(
        out.status.success(),
        "room members --json must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).expect("room members --json must be valid JSON")
}

/// The `{identity_id, role, status}` triples of a roster object, as an
/// order-independent set for convergence comparison.
fn roster_set(roster: &serde_json::Value) -> BTreeSet<(String, String, String)> {
    roster["members"]
        .as_array()
        .expect("roster must carry a members array")
        .iter()
        .map(|m| {
            (
                m["identity_id"].as_str().unwrap_or_default().to_owned(),
                m["role"].as_str().unwrap_or_default().to_owned(),
                m["status"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// Poll `room members --json` on `home` until `member_id` reads `status`, or
/// panic after `timeout` (absorbs a tail child's async persistence window).
fn wait_until_member_status(
    home: &TempDir,
    room: &str,
    member_id: &str,
    status: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let roster = members_json(home, room);
        let matched = roster["members"].as_array().is_some_and(|members| {
            members.iter().any(|m| {
                m["identity_id"].as_str() == Some(member_id) && m["status"].as_str() == Some(status)
            })
        });
        if matched {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "member {member_id} did not reach status {status:?} within {timeout:?}; roster: {roster}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// Reconstruct a real CLI-created identity's `(identity, device)` signing keys
/// from `<home>/identity.secret`, so a CI-tier seed can author events under
/// exactly the identity a subsequent `iroh-rooms` invocation will load
/// (mirrors `two_peer_e2e.rs::every_provider_refused_is_peer_unauthorized`).
fn signing_keys(home: &TempDir) -> (SigningKey, SigningKey) {
    let secret_raw = std::fs::read_to_string(home.path().join("identity.secret"))
        .expect("identity.secret must exist after identity create");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("identity.secret must be valid JSON");
    let seed = |field: &str| -> [u8; 32] {
        let hex_str = secret_v[field].as_str().expect("seed field present");
        <[u8; 32]>::try_from(hex::decode(hex_str).expect("seed is valid hex").as_slice())
            .expect("seed is 32 bytes")
    };
    (
        SigningKey::from_seed(&seed("identity_secret")),
        SigningKey::from_seed(&seed("device_secret")),
    )
}

/// Write `bytes` to `<dir>/<name>` and return the absolute path string
/// (`TempDir` paths are always absolute, satisfying `blob add_path`).
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write fixture file");
    path.to_string_lossy().into_owned()
}

/// Validate `wire` statelessly and insert it into `store`, returning its
/// `event_id`. The single seeding primitive both CI-tier fixtures share.
fn seed(store: &mut EventStore, ctx: &ValidationContext, wire: &WireEvent) -> EventId {
    let validated =
        validate_wire_bytes(&wire.to_bytes(), ctx).expect("event must validate statelessly");
    let id = validated.event_id;
    store.insert(&validated).expect("insert seeded event");
    id
}

// ── ChildSession: a spawned long-running `iroh-rooms` session (ported) ────────

/// A spawned long-running `iroh-rooms` session (`room tail`, `pipe expose`,
/// `pipe connect`). Reader threads drain stdout/stderr into shared buffers so
/// the child never blocks on a full pipe; [`Drop`] kills the child so no
/// orphan survives a panic or early return.
struct ChildSession {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

/// Drain `pipe` line-by-line into `into` (each line kept with a trailing
/// newline so snapshots read naturally). Exits on EOF or a read error.
fn drain(pipe: impl Read + Send + 'static, into: Arc<Mutex<String>>) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            let mut buf = into.lock().expect("capture buffer not poisoned");
            buf.push_str(&line);
            buf.push('\n');
        }
    })
}

impl ChildSession {
    /// Spawn `iroh-rooms <args…>` against `data_dir` with piped stdio and
    /// reader threads. `stdin` is closed so a child never blocks awaiting
    /// input.
    fn spawn(data_dir: &Path, args: &[&str]) -> ChildSession {
        let mut child = Command::new(bin_path())
            .env_remove("IROH_ROOMS_HOME")
            .arg("--data-dir")
            .arg(data_dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn iroh-rooms child session");

        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let out = child.stdout.take().expect("child stdout is piped");
        let err = child.stderr.take().expect("child stderr is piped");
        let readers = vec![
            drain(out, Arc::clone(&stdout)),
            drain(err, Arc::clone(&stderr)),
        ];
        ChildSession {
            child,
            stdout,
            stderr,
            readers,
        }
    }

    /// The first captured line in `buf` containing `needle`, if any.
    fn scan(buf: &Arc<Mutex<String>>, needle: &str) -> Option<String> {
        buf.lock()
            .expect("capture buffer not poisoned")
            .lines()
            .find(|l| l.contains(needle))
            .map(str::to_owned)
    }

    /// Block until a captured **stdout** line contains `needle`, or `timeout`
    /// elapses. Returns the full matching line (so callers can parse it).
    fn wait_for_line(&self, needle: &str, timeout: Duration) -> Result<String, String> {
        self.wait_in(&self.stdout, "stdout", needle, timeout)
    }

    /// Block until a captured **stderr** line contains `needle`, or `timeout`
    /// elapses. Used for the CLI-native pipe denial signals (AC4).
    fn wait_for_stderr_line(&self, needle: &str, timeout: Duration) -> Result<String, String> {
        self.wait_in(&self.stderr, "stderr", needle, timeout)
    }

    fn wait_in(
        &self,
        buf: &Arc<Mutex<String>>,
        stream: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = Self::scan(buf, needle) {
                return Ok(line);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {needle:?} on {stream}\n\
                     --- stdout ---\n{}\n--- stderr ---\n{}",
                    self.stdout_snapshot(),
                    self.stderr_snapshot()
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn stdout_snapshot(&self) -> String {
        self.stdout
            .lock()
            .expect("capture buffer not poisoned")
            .clone()
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr
            .lock()
            .expect("capture buffer not poisoned")
            .clone()
    }
}

impl Drop for ChildSession {
    fn drop(&mut self) {
        // SIGKILL is the portable, unsafe-free stop (the workspace forbids
        // `unsafe`). Temp homes are discarded at test end, so a pipe lingering
        // open until GC is irrelevant; where a clean `pipe.closed` matters
        // (the restart-validation narrative), the suite issues an explicit
        // `pipe close` one-shot instead of relying on signal teardown.
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in std::mem::take(&mut self.readers) {
            let _ = reader.join();
        }
    }
}

// ── in-test loopback echo target (the pipe-tier "service being exposed") ──────

/// A loopback TCP echo server (the `--tcp` forward target for the pipe tier).
/// Returns its address and a counter of accepted connections, so a denial
/// test can prove the owner never connected to it.
async fn spawn_echo_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind echo server");
    let addr = listener.local_addr().expect("echo server addr");
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = Arc::clone(&count);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            count2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, count)
}

// ── harness / parser self-guard unit tests (CI tier) ───────────────────────────
//
// The helpers above are called by every online-tier test. A bug in any of
// them would silently corrupt the online-tier assertions, so they get their
// own deterministic, network-free coverage here (ported from `two_peer_e2e.rs`).

#[test]
fn extract_field_finds_value_after_colon() {
    let output = "room_id: blake3:aabb\nstored: yes\n";
    assert_eq!(extract_field(output, "room_id"), Some("blake3:aabb"));
    assert_eq!(extract_field(output, "stored"), Some("yes"));
}

#[test]
fn extract_ticket_finds_roomtkt1_token() {
    let output = "invite_id: abc\nticket:\n  roomtkt1sometoken\nrole: member\n";
    assert_eq!(extract_ticket(output), Some("roomtkt1sometoken"));
}

#[test]
fn parse_listening_extracts_address() {
    let line = "listening: abcdef0123456789@127.0.0.1:54321";
    assert_eq!(parse_listening(line), "abcdef0123456789@127.0.0.1:54321");
}

#[test]
fn parse_forwarding_extracts_socket_addr() {
    let line = "forwarding: 127.0.0.1:9999 -> pipe abcdef";
    let addr = parse_forwarding(line);
    assert_eq!(
        addr,
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 9999))
    );
}

/// `roster_set` must produce equal sets regardless of array order — the
/// three-way convergence assertion depends on it.
#[test]
fn roster_set_is_order_independent() {
    let roster_ab: serde_json::Value = serde_json::from_str(
        r#"{"admin":"aaa","members":[
            {"identity_id":"aaa","role":"admin","status":"active"},
            {"identity_id":"bbb","role":"member","status":"active"}
        ]}"#,
    )
    .expect("roster_ab is valid JSON");
    let roster_ba: serde_json::Value = serde_json::from_str(
        r#"{"admin":"aaa","members":[
            {"identity_id":"bbb","role":"member","status":"active"},
            {"identity_id":"aaa","role":"admin","status":"active"}
        ]}"#,
    )
    .expect("roster_ba is valid JSON");
    assert_eq!(
        roster_set(&roster_ab),
        roster_set(&roster_ba),
        "roster_set must be order-independent across the members array"
    );
}

/// Harness self-guard: a `ChildSession` captures a spawned child's stdout.
#[test]
fn child_session_captures_output() {
    let home = TempDir::new().unwrap();
    let session = ChildSession::spawn(home.path(), &["identity", "create", "--name", "Harness"]);
    session
        .wait_for_line("identity_id:", WAIT)
        .expect("ChildSession must capture the child's stdout");
}

/// Harness self-guard for stderr: AC4 depends on `wait_for_stderr_line`
/// finding `pipe.connect.rejected:not_allowed` and `[pipe] denied by the
/// owner` — if the stderr drain is broken, those signals are silently lost.
#[test]
fn child_session_captures_stderr() {
    let home = TempDir::new().unwrap();
    let session = ChildSession::spawn(
        home.path(),
        &["room", "tail", "not-a-valid-id", "--offline"],
    );
    session
        .wait_for_stderr_line("invalid", Duration::from_secs(5))
        .expect("ChildSession must capture the child's stderr for failing commands");
}

// ══ CI tier (deterministic, network-free) ═════════════════════════════════════

/// AC1 — the full offline backbone (three identities, room create, a human
/// invite plus an agent invite, offline send, offline reads) succeeds with
/// only local `--data-dir` stores and no server started by the harness. The
/// whole three-party product slice is local-first: no relay, broker, or
/// application server is anywhere in the loop.
#[test]
fn full_slice_runs_without_central_server() {
    let alice = TempDir::new().unwrap();
    let bob = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    identity_create(&alice, "Alice");
    identity_create(&bob, "Bob");
    identity_create(&agent, "build-agent");
    let alice_id = identity_id(&alice);
    let bob_id = identity_id(&bob);
    let agent_id = identity_id(&agent);
    let room = room_create(&alice, "Local First Room");

    // Both invites are out-of-band capability paths — minted locally, no server.
    let bob_ticket = invite(&alice, &room, &bob_id, "member", None);
    assert!(
        bob_ticket.starts_with("roomtkt1"),
        "the human invite must mint a roomtkt1… ticket; got: {bob_ticket}"
    );
    let agent_ticket = agent_invite(&alice, &room, &agent_id);
    assert!(
        agent_ticket.starts_with("roomtkt1"),
        "the agent invite must mint a roomtkt1… ticket; got: {agent_ticket}"
    );

    // Offline-first send: stored locally, delivered to nobody (no peers to reach).
    let send = one_shot(alice.path(), &["room", "send", &room, "local-first hello"]);
    assert!(
        send.status.success(),
        "offline send must exit 0; stderr: {}",
        String::from_utf8_lossy(&send.stderr)
    );
    let send_out = String::from_utf8_lossy(&send.stdout);
    assert_eq!(
        extract_field(&send_out, "stored"),
        Some("yes"),
        "offline send must be stored locally"
    );
    assert!(
        send_out.contains("delivered: 0"),
        "offline send reaches no peers; got:\n{send_out}"
    );

    // Offline reads succeed with the network unused.
    let roster = members_json(&alice, &room);
    assert_eq!(
        roster["admin"].as_str(),
        Some(alice_id.as_str()),
        "the room admin must be Alice"
    );
    assert!(
        roster_set(&roster).contains(&(alice_id, "admin".to_owned(), "active".to_owned())),
        "Alice must read as admin/active in her own roster"
    );
    let read = one_shot(
        alice.path(),
        &["room", "tail", &room, "--offline", "--json"],
    );
    assert!(
        read.status.success(),
        "offline tail read must exit 0; stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
}

/// Every MVP `event_type` the demo produces, in the order this fixture authors
/// them (spec D5). `member.invited`/`member.joined` each appear twice (Bob,
/// then the Agent), so 12 authored events cover 10 distinct types.
const FULL_EVENT_TYPES: [&str; 12] = [
    "room.created",
    "member.invited",
    "member.invited",
    "member.joined",
    "member.joined",
    "message.text",
    "file.shared",
    "pipe.opened",
    "pipe.closed",
    "agent.status",
    "member.left",
    "member.removed",
];

const FULL_ADMIN_SEED: [u8; 32] = [0x30; 32];
const FULL_ADMIN_DEV_SEED: [u8; 32] = [0x31; 32];
const FULL_BOB_SEED: [u8; 32] = [0x34; 32];
const FULL_BOB_DEV_SEED: [u8; 32] = [0x35; 32];
const FULL_AGENT_SEED: [u8; 32] = [0x38; 32];
const FULL_AGENT_DEV_SEED: [u8; 32] = [0x39; 32];
const FULL_ROOM_NONCE: [u8; 16] = [0xdd; 16];
const FULL_BOB_INVITE_ID: [u8; 16] = [0xe1; 16];
const FULL_AGENT_INVITE_ID: [u8; 16] = [0xe2; 16];
const FULL_BOB_CAP_SECRET: [u8; 16] = [0x61; 16];
const FULL_AGENT_CAP_SECRET: [u8; 16] = [0x62; 16];
const FULL_FILE_ID: [u8; 16] = [0xf1; 16];
const FULL_PIPE_ID: [u8; 16] = [0xf2; 16];
const FULL_BASE_TS: u64 = 1_750_200_000_000;

/// Seed a `rooms.db` in `home` with one event of every MVP type (spec §2.3 /
/// D5), fully network-free and deterministic (fixed seeds, no `identity
/// create` needed — the offline read commands require no local secret). The
/// chain: genesis → invite(Bob) → invite(Agent) → join(Bob) → join(Agent) →
/// message → file.shared → pipe.opened → pipe.closed → agent.status →
/// member.left(Bob) → member.removed(Agent). Returns the room id string.
#[allow(clippy::too_many_lines)] // one linear 12-event seed chain; splitting fragments it
fn seed_full_event_type_chain(home: &TempDir) -> String {
    let admin_identity = SigningKey::from_seed(&FULL_ADMIN_SEED);
    let admin_device = SigningKey::from_seed(&FULL_ADMIN_DEV_SEED);
    let bob_identity = SigningKey::from_seed(&FULL_BOB_SEED);
    let bob_device = SigningKey::from_seed(&FULL_BOB_DEV_SEED);
    let agent_identity = SigningKey::from_seed(&FULL_AGENT_SEED);
    let agent_device = SigningKey::from_seed(&FULL_AGENT_DEV_SEED);

    let room_id = signed::derive_room_id(
        &admin_identity.identity_key(),
        &FULL_ROOM_NONCE,
        FULL_BASE_TS,
    );
    let ctx = ValidationContext::for_room(room_id);
    let mut store = EventStore::open(&home.path().join("rooms.db")).expect("open store to seed");

    // 1. room.created (Alice/admin).
    let genesis_id = seed(
        &mut store,
        &ctx,
        &build_room_created(
            &admin_identity,
            &admin_device,
            "Full Demo Room",
            &FULL_ROOM_NONCE,
            FULL_BASE_TS,
        ),
    );

    // 2/3. member.invited (Bob, member) + member.invited (Agent, agent) —
    // concurrent, both cite genesis.
    let ts = FULL_BASE_TS + 1_000;
    let bob_cap_hash = capability_hash(&room_id, &FULL_BOB_INVITE_ID, &FULL_BOB_CAP_SECRET);
    let invite_bob_id = seed(
        &mut store,
        &ctx,
        &build_member_invited(
            &admin_identity,
            &admin_device,
            &room_id,
            &FULL_BOB_INVITE_ID,
            &bob_cap_hash,
            "member",
            &bob_identity.identity_key(),
            None,
            None,
            &[genesis_id],
            ts,
        ),
    );
    let agent_cap_hash = capability_hash(&room_id, &FULL_AGENT_INVITE_ID, &FULL_AGENT_CAP_SECRET);
    let invite_agent_id = seed(
        &mut store,
        &ctx,
        &build_member_invited(
            &admin_identity,
            &admin_device,
            &room_id,
            &FULL_AGENT_INVITE_ID,
            &agent_cap_hash,
            "agent",
            &agent_identity.identity_key(),
            None,
            None,
            &[genesis_id],
            ts,
        ),
    );

    // 4/5. member.joined (Bob) + member.joined (Agent).
    let ts = ts + 1_000;
    let bob_binding = DeviceBinding::create(&room_id, &bob_identity, bob_device.device_key());
    let join_bob_id = seed(
        &mut store,
        &ctx,
        &build_member_joined(
            &bob_identity,
            &bob_device,
            &room_id,
            &FULL_BOB_INVITE_ID,
            &FULL_BOB_CAP_SECRET,
            "member",
            bob_binding,
            Some("Bob"),
            &[invite_bob_id],
            ts,
        ),
    );
    let agent_binding = DeviceBinding::create(&room_id, &agent_identity, agent_device.device_key());
    let join_agent_id = seed(
        &mut store,
        &ctx,
        &build_member_joined(
            &agent_identity,
            &agent_device,
            &room_id,
            &FULL_AGENT_INVITE_ID,
            &FULL_AGENT_CAP_SECRET,
            "agent",
            agent_binding,
            Some("build-agent"),
            &[invite_agent_id],
            ts,
        ),
    );

    // 6. message.text (Bob).
    let ts = ts + 1_000;
    let message_id = seed(
        &mut store,
        &ctx,
        &build_message_text(
            &bob_identity,
            &bob_device,
            &room_id,
            "prototype is up",
            None,
            None,
            &[],
            &[join_bob_id, join_agent_id],
            ts,
        ),
    );

    // 7. file.shared (Bob shares; Bob's device is the sole provider).
    let ts = ts + 1_000;
    let file_event_id = seed(
        &mut store,
        &ctx,
        &build_file_shared(
            &bob_identity,
            &bob_device,
            &room_id,
            FULL_FILE_ID,
            "demo.txt",
            "text/plain",
            11,
            HashRef::from_bytes([0xABu8; 32]),
            Some("raw"),
            &[bob_device.device_key()],
            &[message_id],
            ts,
        ),
    );

    // 8. pipe.opened (Alice exposes, allowing Bob).
    let ts = ts + 1_000;
    let pipe_open_id = seed(
        &mut store,
        &ctx,
        &build_pipe_opened(
            &admin_identity,
            &admin_device,
            &room_id,
            FULL_PIPE_ID,
            &admin_device.device_key(),
            "demo-pipe",
            "127.0.0.1:9999",
            "/iroh-rooms/pipe/1",
            &[bob_identity.identity_key()],
            None,
            &[file_event_id],
            ts,
        ),
    );

    // 9. pipe.closed (Alice closes it cleanly).
    let ts = ts + 1_000;
    let pipe_close_id = seed(
        &mut store,
        &ctx,
        &build_pipe_closed(
            &admin_identity,
            &admin_device,
            &room_id,
            FULL_PIPE_ID,
            Some("closed"),
            &[pipe_open_id],
            ts,
        ),
    );

    // 10. agent.status (the Agent posts progress).
    let ts = ts + 1_000;
    let status_id = seed(
        &mut store,
        &ctx,
        &build_agent_status(
            &agent_identity,
            &agent_device,
            &room_id,
            "running_tests",
            Some("suite in progress"),
            &[],
            Some(40),
            &[pipe_close_id],
            ts,
        ),
    );

    // 11. member.left (Bob voluntarily departs).
    let ts = ts + 1_000;
    let left_id = seed(
        &mut store,
        &ctx,
        &build_member_left(&bob_identity, &bob_device, &room_id, None, &[status_id], ts),
    );

    // 12. member.removed (Alice removes the Agent).
    let ts = ts + 1_000;
    seed(
        &mut store,
        &ctx,
        &build_member_removed(
            &admin_identity,
            &admin_device,
            &room_id,
            &agent_identity.identity_key(),
            None,
            None,
            &[left_id],
            ts,
        ),
    );

    room_id.to_string()
}

/// AC2, full strength: a `rooms.db` seeded with one event of **every** MVP
/// type re-validates and re-folds byte-stably across a cold restart. A fresh
/// `room tail --offline --json` process is the restart: (a) every expected
/// `event_type` is present, (b) the projected row count equals the authored
/// count (nothing silently dropped as invalid on reload — the honest form of
/// "all events validate"), and (c) two cold reads are byte-identical (fold
/// determinism across restart).
#[test]
fn all_event_types_validate_after_restart() {
    let home = TempDir::new().unwrap();
    let room = seed_full_event_type_chain(&home);

    let read1 = one_shot(home.path(), &["room", "tail", &room, "--offline", "--json"]);
    assert!(
        read1.status.success(),
        "offline tail --json must succeed; stderr: {}",
        String::from_utf8_lossy(&read1.stderr)
    );
    let rows1: serde_json::Value =
        serde_json::from_slice(&read1.stdout).expect("tail --json emits a JSON array");
    let array1 = rows1.as_array().expect("tail --json is an array");

    assert_eq!(
        array1.len(),
        FULL_EVENT_TYPES.len(),
        "projected row count must equal the authored count (nothing silently \
         dropped as invalid on reload): {rows1}"
    );

    for expected_type in FULL_EVENT_TYPES {
        let want = FULL_EVENT_TYPES
            .iter()
            .filter(|t| **t == expected_type)
            .count();
        let got = array1
            .iter()
            .filter(|r| r["event_type"].as_str() == Some(expected_type))
            .count();
        assert_eq!(
            got, want,
            "event_type {expected_type:?}: expected {want} occurrence(s), found {got}; rows: {rows1}"
        );
    }

    // Fold determinism / byte-stability across a second cold read.
    let read2 = one_shot(home.path(), &["room", "tail", &room, "--offline", "--json"]);
    assert!(read2.status.success(), "second restart read must exit 0");
    assert_eq!(
        read1.stdout, read2.stdout,
        "the offline JSON read must be byte-stable across restarts"
    );
}

/// AC2, content strength (spec R5) — "all events validate after restart" is only
/// meaningful if the *content* of each event, not merely its type and count,
/// survives the cold reload's re-validate → re-fold → re-render. Its sibling
/// [`all_event_types_validate_after_restart`] proves the structural half
/// (every type present, count exact, byte-stable); this proves the semantic
/// half. The sibling `tail_cli.rs` asserts content survival only for
/// `room.created` (room name) and `message.text` (body) — no suite asserts it
/// for the Blob / Pipe / Agent event bodies, so a projection regression that
/// silently blanked, say, the `file.shared` hash or the `agent.status` state on
/// reload would pass every existing restart test. This closes that gap for the
/// full-demo event set: after a fresh `room tail --offline --json` process
/// reads the builder-seeded chain, each diverse event's load-bearing fields
/// read back exactly as authored.
#[test]
#[allow(clippy::too_many_lines)] // one linear per-event-type content audit; splitting fragments it
fn seeded_event_content_survives_restart() {
    let home = TempDir::new().unwrap();
    let room = seed_full_event_type_chain(&home);

    let read = one_shot(home.path(), &["room", "tail", &room, "--offline", "--json"]);
    assert!(
        read.status.success(),
        "offline tail --json must succeed; stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&read.stdout).expect("tail --json emits a JSON array");
    let array = rows.as_array().expect("tail --json is an array");

    // The single row of a given event type (each type inspected below is
    // authored exactly once by the seed chain).
    let row_of = |event_type: &str| -> &serde_json::Value {
        let matches: Vec<&serde_json::Value> = array
            .iter()
            .filter(|r| r["event_type"].as_str() == Some(event_type))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one {event_type} row; rows: {rows}"
        );
        matches[0]
    };

    // message.text — the body survives verbatim and `format` reads back as the
    // omit-when-empty default (`plain`).
    let message = row_of("message.text");
    assert_eq!(
        message["body"].as_str(),
        Some("prototype is up"),
        "message.text body must survive restart; row: {message}"
    );
    assert_eq!(
        message["format"].as_str(),
        Some("plain"),
        "message.text format must default to plain on read; row: {message}"
    );

    // file.shared — name, size, and the BLAKE3 blob hash survive verbatim. The
    // hash is exactly what a live `file fetch` independently re-verifies against
    // (AC3), so its survival across restart is load-bearing.
    let file = row_of("file.shared");
    assert_eq!(
        file["file_name"].as_str(),
        Some("demo.txt"),
        "file.shared name must survive restart; row: {file}"
    );
    assert_eq!(
        file["size_bytes"].as_u64(),
        Some(11),
        "file.shared size must survive restart; row: {file}"
    );
    let expected_hash = HashRef::from_bytes([0xABu8; 32]).to_string();
    assert_eq!(
        file["blob_hash"].as_str(),
        Some(expected_hash.as_str()),
        "file.shared blob hash must survive restart; row: {file}"
    );

    // agent.status — the state, human message, and progress the agent posted all
    // survive (AC5's status content, network-free).
    let status = row_of("agent.status");
    assert_eq!(
        status["state"].as_str(),
        Some("running_tests"),
        "agent.status state must survive restart; row: {status}"
    );
    assert_eq!(
        status["message"].as_str(),
        Some("suite in progress"),
        "agent.status message must survive restart; row: {status}"
    );
    assert_eq!(
        status["progress"].as_u64(),
        Some(40),
        "agent.status progress must survive restart; row: {status}"
    );

    // pipe.opened / pipe.closed — a matched open/close pair carrying the same
    // pipe id, so the restarted log still reads as one cleanly-closed pipe (spec
    // R6: the explicit close event, not a SIGKILL, is what leaves this pair).
    let expected_pipe_id = hex::encode(FULL_PIPE_ID);
    assert_eq!(
        row_of("pipe.opened")["pipe_id"].as_str(),
        Some(expected_pipe_id.as_str()),
        "pipe.opened must carry the authored pipe id after restart"
    );
    assert_eq!(
        row_of("pipe.closed")["pipe_id"].as_str(),
        Some(expected_pipe_id.as_str()),
        "pipe.closed must reference the same pipe id after restart"
    );

    // member.invited ×2 — the two invites keep their distinct roles across the
    // reload: the human was invited as a `member`, the agent as an `agent`
    // (AC5's least-privilege invitation, durable after restart).
    let invited_roles: BTreeSet<String> = array
        .iter()
        .filter(|r| r["event_type"].as_str() == Some("member.invited"))
        .filter_map(|r| r["invited_role"].as_str().map(str::to_owned))
        .collect();
    let expected_roles: BTreeSet<String> = ["agent".to_owned(), "member".to_owned()]
        .into_iter()
        .collect();
    assert_eq!(
        invited_roles, expected_roles,
        "both invite roles (member + agent) must survive restart; rows: {rows}"
    );

    // member.removed — the admin-removal keeps both the removed subject (the
    // agent) and the acting admin, distinctly, after restart. Reconstructed from
    // the same deterministic seeds the chain was authored under.
    let removed = row_of("member.removed");
    let expected_subject = SigningKey::from_seed(&FULL_AGENT_SEED)
        .identity_key()
        .to_string();
    let expected_admin = SigningKey::from_seed(&FULL_ADMIN_SEED)
        .identity_key()
        .to_string();
    assert_ne!(
        expected_subject, expected_admin,
        "sanity: the removed subject and the acting admin are distinct identities"
    );
    assert_eq!(
        removed["subject"].as_str(),
        Some(expected_subject.as_str()),
        "member.removed must name the removed agent after restart; row: {removed}"
    );
    assert_eq!(
        removed["removed_by"].as_str(),
        Some(expected_admin.as_str()),
        "member.removed must name the acting admin after restart; row: {removed}"
    );
}

/// AC2 + membership-fold — the demo's **departure** events must not merely
/// persist in the offline log projection (which
/// [`all_event_types_validate_after_restart`] and
/// [`seeded_event_content_survives_restart`] already cover); they must actually
/// *take effect* in the membership fold after a cold reload. This folds the same
/// full builder-seeded chain — which ends with `member.left(Bob)` then
/// `member.removed(Agent)` — through a fresh `room members --json` process (the
/// restart) and asserts the post-lifecycle roster: Alice, the room's single
/// immutable admin, stays `admin`/`active`; Bob reads `member`/`left` (a
/// voluntary self-leave); the Agent reads `agent`/`removed` (an admin removal) —
/// neither departed member reads `active`, and neither is elevated. It is the
/// deterministic, network-free complement to the online
/// [`three_way_membership_converges`], which only ever asserts the all-active
/// *pre*-departure roster and so never exercises the `left`/`removed`
/// distinction or admin immutability across the whole cast.
#[test]
fn seeded_full_chain_membership_fold_reflects_departures() {
    let home = TempDir::new().unwrap();
    let room = seed_full_event_type_chain(&home);

    // A fresh process folds the persisted log — the restart.
    let roster = members_json(&home, &room);

    // Identity ids are the string form of each seed's identity key (the same
    // mapping the offline projection uses for `member.removed`'s subject).
    let alice_id = SigningKey::from_seed(&FULL_ADMIN_SEED)
        .identity_key()
        .to_string();
    let bob_id = SigningKey::from_seed(&FULL_BOB_SEED)
        .identity_key()
        .to_string();
    let agent_id = SigningKey::from_seed(&FULL_AGENT_SEED)
        .identity_key()
        .to_string();

    // The whole cast is still known to the fold (a departure strips capability,
    // not the row): admin/active, member/left, agent/removed. The `left` vs
    // `removed` distinction proves the terminal departure *kind* survives the
    // reload, not just a collapsed `Status::Removed`.
    let expected: BTreeSet<(String, String, String)> = [
        (alice_id.clone(), "admin".to_owned(), "active".to_owned()),
        (bob_id, "member".to_owned(), "left".to_owned()),
        (agent_id, "agent".to_owned(), "removed".to_owned()),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        roster_set(&roster),
        expected,
        "the folded roster must reflect the full lifecycle (Bob left, Agent removed, \
         Alice admin) after restart; roster: {roster}"
    );

    // Admin immutability + least privilege: the acting admin is exactly Alice,
    // and she is the sole member left `active` once both departures collapse the
    // active set — a member.left / member.removed cannot elevate anyone.
    assert_eq!(
        roster["admin"].as_str(),
        Some(alice_id.as_str()),
        "the room admin must remain Alice after the departures; roster: {roster}"
    );
    let members = roster["members"]
        .as_array()
        .expect("roster must carry a members array");
    let active: Vec<&str> = members
        .iter()
        .filter(|m| m["status"].as_str() == Some("active"))
        .filter_map(|m| m["identity_id"].as_str())
        .collect();
    assert_eq!(
        active,
        vec![alice_id.as_str()],
        "only the immutable admin stays active once Bob leaves and the Agent is \
         removed; roster: {roster}"
    );
    let admins: Vec<&str> = members
        .iter()
        .filter(|m| m["is_admin"].as_bool() == Some(true))
        .filter_map(|m| m["identity_id"].as_str())
        .collect();
    assert_eq!(
        admins,
        vec![alice_id.as_str()],
        "exactly one member — Alice — carries the admin flag; no departed member is \
         elevated; roster: {roster}"
    );
}

const AC5_ADMIN_SEED: [u8; 32] = [0x20; 32];
const AC5_ADMIN_DEV_SEED: [u8; 32] = [0x21; 32];
const AC5_ROOM_NONCE: [u8; 16] = [0xcc; 16];
const AC5_INVITE_ID: [u8; 16] = [0xea; 16];
const AC5_CAP_SECRET: [u8; 16] = [0x5f; 16];
const AC5_BASE_TS: u64 = 1_750_100_000_000;

/// AC5 — the agent posts a status but gains no implicit extra privilege.
/// Builder-seeds a room directly into a **real** CLI identity's own
/// `rooms.db` (so `agent status`/`room invite`/`agent invite` load matching
/// keys) with the agent as an active `role: agent` member: (1) posting a
/// status succeeds — `gate_active_member` is the only gate, not role; (2) the
/// agent cannot invite anyone — it is not the room's single immutable admin,
/// so membership grants exactly "active member," nothing more; (3) the fold
/// shows the agent's own role as `agent`, never `admin`/`member` (the
/// `Agent < Member < Admin` lattice).
#[test]
#[allow(clippy::too_many_lines)] // one linear seed-post-refuse-check narrative; splitting fragments it
fn agent_posts_status_but_has_no_admin_privilege() {
    let agent_home = TempDir::new().unwrap();
    identity_create(&agent_home, "build-agent");
    let (agent_identity, agent_device) = signing_keys(&agent_home);

    let admin_identity = SigningKey::from_seed(&AC5_ADMIN_SEED);
    let admin_device = SigningKey::from_seed(&AC5_ADMIN_DEV_SEED);

    let room_id =
        signed::derive_room_id(&admin_identity.identity_key(), &AC5_ROOM_NONCE, AC5_BASE_TS);
    let ctx = ValidationContext::for_room(room_id);
    let mut store =
        EventStore::open(&agent_home.path().join("rooms.db")).expect("open agent's store to seed");

    let genesis_id = seed(
        &mut store,
        &ctx,
        &build_room_created(
            &admin_identity,
            &admin_device,
            "AC5 Room",
            &AC5_ROOM_NONCE,
            AC5_BASE_TS,
        ),
    );
    let cap_hash = capability_hash(&room_id, &AC5_INVITE_ID, &AC5_CAP_SECRET);
    let invite_id = seed(
        &mut store,
        &ctx,
        &build_member_invited(
            &admin_identity,
            &admin_device,
            &room_id,
            &AC5_INVITE_ID,
            &cap_hash,
            "agent",
            &agent_identity.identity_key(),
            None,
            None,
            &[genesis_id],
            AC5_BASE_TS + 1_000,
        ),
    );
    let binding = DeviceBinding::create(&room_id, &agent_identity, agent_device.device_key());
    seed(
        &mut store,
        &ctx,
        &build_member_joined(
            &agent_identity,
            &agent_device,
            &room_id,
            &AC5_INVITE_ID,
            &AC5_CAP_SECRET,
            "agent",
            binding,
            Some("build-agent"),
            &[invite_id],
            AC5_BASE_TS + 2_000,
        ),
    );
    drop(store);

    let room = room_id.to_string();

    // Positive: any active member — including the agent — may post a status.
    let status = one_shot(
        agent_home.path(),
        &[
            "agent",
            "status",
            &room,
            "running_tests",
            "--message",
            "no special privilege needed to post this",
            "--progress",
            "10",
        ],
    );
    assert!(
        status.status.success(),
        "agent status from an active agent-role member must exit 0; stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert_eq!(
        extract_field(&status_out, "stored"),
        Some("yes"),
        "the status must be stored locally: {status_out}"
    );

    // Negative: the agent is not the admin, so it cannot invite anyone through
    // either invite surface — no implicit extra privilege (current binary
    // behavior: the admin-only pre-check is an uncoded failure, exit 1, not a
    // Reject-derived Auth code; asserted against the actual behavior per the
    // spec's binary-is-source-of-truth rule, mirroring `invite_cli.rs` /
    // `agent_cli.rs`'s own non-admin rejection tests).
    let bogus_invitee = "c1".repeat(32);
    let invite_attempt = one_shot(
        agent_home.path(),
        &[
            "room",
            "invite",
            &room,
            "--invitee",
            &bogus_invitee,
            "--role",
            "member",
        ],
    );
    assert!(
        !invite_attempt.status.success(),
        "a non-admin agent must not be able to invite"
    );
    let invite_stderr = String::from_utf8_lossy(&invite_attempt.stderr);
    assert!(
        invite_stderr.contains("admin"),
        "the refusal must name the admin-only requirement; stderr: {invite_stderr}"
    );

    let agent_invite_attempt = one_shot(
        agent_home.path(),
        &["agent", "invite", &room, &bogus_invitee],
    );
    assert!(
        !agent_invite_attempt.status.success(),
        "a non-admin agent must not be able to issue an agent invite either"
    );
    assert!(
        String::from_utf8_lossy(&agent_invite_attempt.stderr).contains("admin"),
        "the agent-invite refusal must also name the admin-only requirement"
    );

    // Least privilege in the fold: the agent reads its own role as `agent`,
    // never `admin`/`member` (spike §3.5; PRD §13.3).
    let agent_id = identity_id(&agent_home);
    let roster = members_json(&agent_home, &room);
    let role = roster["members"]
        .as_array()
        .expect("roster must carry a members array")
        .iter()
        .find(|m| m["identity_id"].as_str() == Some(agent_id.as_str()))
        .and_then(|m| m["role"].as_str())
        .map(str::to_owned);
    assert_eq!(
        role.as_deref(),
        Some("agent"),
        "the agent must read its own role as `agent`, never admin/member: {roster}"
    );
}

// ══ Online tier (three live loopback processes; #[ignore]-gated) ═════════════

/// A converged three-member `{Alice, Bob, Agent}` room across three isolated
/// homes.
struct Converged3 {
    alice_home: TempDir,
    bob_home: TempDir,
    agent_home: TempDir,
    room: String,
    alice_id: String,
    bob_id: String,
    agent_id: String,
}

/// Drive the full three-party membership handshake over loopback: Alice hosts
/// `room tail --accept-joins`, Bob redeems his ticket, then the Agent redeems
/// its ticket (both dialing Alice directly). Bob joined before the Agent
/// existed in the room, so his own join bootstrap never observed the Agent's
/// later join; a follow-up connection to Alice pulls the bounded
/// recent-history sync (IR-0201) that catches him up, so a genuine three-way
/// roster comparison after this helper returns is not vacuous. Alice's
/// session is stopped before returning.
fn converge_three_member_room() -> Converged3 {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let agent_home = TempDir::new().expect("agent home");
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    identity_create(&agent_home, "build-agent");
    let alice_id = identity_id(&alice_home);
    let bob_id = identity_id(&bob_home);
    let agent_id = identity_id(&agent_home);
    let room = room_create(&alice_home, "Three-Party Room");
    let bob_ticket = invite(&alice_home, &room, &bob_id, "member", Some("24h"));
    let agent_ticket = agent_invite(&alice_home, &room, &agent_id);

    let alice_tail = ChildSession::spawn(
        alice_home.path(),
        &["room", "tail", &room, "--accept-joins", "--loopback"],
    );
    let listening = alice_tail
        .wait_for_line("listening:", WAIT)
        .unwrap_or_else(|err| panic!("alice tail never advertised a listening address: {err}"));
    let alice_addr = parse_listening(&listening);

    let bob_join = one_shot(
        bob_home.path(),
        &[
            "room",
            "join",
            &bob_ticket,
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    assert!(
        bob_join.status.success(),
        "bob join must succeed; stderr: {}",
        String::from_utf8_lossy(&bob_join.stderr)
    );
    assert!(
        String::from_utf8_lossy(&bob_join.stdout).contains("members: 2 active"),
        "bob join must report a 2-member room"
    );

    let agent_join = one_shot(
        agent_home.path(),
        &[
            "room",
            "join",
            &agent_ticket,
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    assert!(
        agent_join.status.success(),
        "agent join must succeed; stderr: {}",
        String::from_utf8_lossy(&agent_join.stderr)
    );
    assert!(
        String::from_utf8_lossy(&agent_join.stdout).contains("members: 3 active"),
        "agent join must report a 3-member room"
    );

    wait_until_member_status(&alice_home, &room, &bob_id, "active", WAIT);
    wait_until_member_status(&alice_home, &room, &agent_id, "active", WAIT);

    // Bob's settle round-trip (see doc comment above).
    let settle = one_shot(
        bob_home.path(),
        &[
            "room",
            "send",
            &room,
            "sync",
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    assert!(
        settle.status.success(),
        "bob's settle send must succeed; stderr: {}",
        String::from_utf8_lossy(&settle.stderr)
    );

    drop(alice_tail);

    Converged3 {
        alice_home,
        bob_home,
        agent_home,
        room,
        alice_id,
        bob_id,
        agent_id,
    }
}

/// Support oracle for AC1/AC3/AC4: all three homes agree on room membership
/// after the join handshake. Both peers *and* the agent converge on the same
/// admin, and the same `{identity_id, role, status}` set.
#[test]
#[ignore = "three live loopback processes; run with --ignored --test-threads=1"]
fn three_way_membership_converges() {
    let c = converge_three_member_room();

    let alice_roster = members_json(&c.alice_home, &c.room);
    let bob_roster = members_json(&c.bob_home, &c.room);
    let agent_roster = members_json(&c.agent_home, &c.room);

    assert_eq!(
        alice_roster["admin"], bob_roster["admin"],
        "alice and bob must agree on the admin"
    );
    assert_eq!(
        alice_roster["admin"], agent_roster["admin"],
        "alice and the agent must agree on the admin"
    );
    assert_eq!(
        alice_roster["admin"].as_str(),
        Some(c.alice_id.as_str()),
        "the agreed admin must be Alice"
    );

    let expected: BTreeSet<(String, String, String)> = [
        (c.alice_id.clone(), "admin".to_owned(), "active".to_owned()),
        (c.bob_id.clone(), "member".to_owned(), "active".to_owned()),
        (c.agent_id.clone(), "agent".to_owned(), "active".to_owned()),
    ]
    .into_iter()
    .collect();
    let alice_set = roster_set(&alice_roster);
    let bob_set = roster_set(&bob_roster);
    let agent_set = roster_set(&agent_roster);
    assert_eq!(
        alice_set, bob_set,
        "alice and bob must converge on the same membership set"
    );
    assert_eq!(
        alice_set, agent_set,
        "alice and the agent must converge on the same membership set"
    );
    assert_eq!(
        alice_set, expected,
        "the converged roster must be {{Alice admin/active, Bob member/active, Agent agent/active}}"
    );
}

/// AC4 (positive half) — the pipe forwards bytes for an authorized peer. In
/// the converged three-party room, Alice exposes a loopback echo target
/// allowing Bob; Bob connects and a `ping` round-trips through the pipe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "three live loopback processes; run with --ignored --test-threads=1"]
async fn authorized_pipe_forwards_bytes_three_party() {
    let c = converge_three_member_room();
    let (echo_addr, echo_count) = spawn_echo_server().await;
    let echo_str = echo_addr.to_string();

    let expose = ChildSession::spawn(
        c.alice_home.path(),
        &[
            "pipe",
            "expose",
            &c.room,
            "--tcp",
            &echo_str,
            "--allow",
            &c.bob_id,
            "--loopback",
        ],
    );
    let pipe_line = expose
        .wait_for_line("pipe_id:", WAIT)
        .unwrap_or_else(|err| panic!("pipe expose never announced a pipe_id: {err}"));
    let pipe_id = extract_field(&pipe_line, "pipe_id")
        .expect("pipe expose must print a pipe_id")
        .to_owned();
    let expose_stdout = expose.stdout_snapshot();
    let listening = expose_stdout
        .lines()
        .find(|l| l.contains("listening:"))
        .expect("pipe expose must print a listening address");
    let alice_addr = parse_listening(listening);

    let connect = ChildSession::spawn(
        c.bob_home.path(),
        &[
            "pipe",
            "connect",
            &c.room,
            &pipe_id,
            "--local",
            "0",
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    let forwarding = connect
        .wait_for_line("forwarding:", WAIT)
        .unwrap_or_else(|err| panic!("pipe connect never bound a local port: {err}"));
    let local = parse_forwarding(&forwarding);

    let echoed = tokio::time::timeout(WAIT, async {
        let mut client = TcpStream::connect(local).await.expect("connect local port");
        client.write_all(b"ping").await.expect("write ping");
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.expect("read echo");
        buf
    })
    .await
    .expect("authorized round-trip within budget");

    assert_eq!(
        &echoed, b"ping",
        "AC4: bytes must echo back through the pipe"
    );
    assert!(
        echo_count.load(Ordering::SeqCst) >= 1,
        "AC4: the owner must have connected to the echo target for bob"
    );
}

/// AC4 (negative half) — an authorized-vs-denied contrast that falls out of
/// the three-party cast for free (spec D8): Alice exposes allowing **Bob
/// only**. The Agent is an Active member of the same room but is not on the
/// allow-list, so its connect passes the CLI's active-member pre-check yet is
/// denied owner-side. Proof: no bytes round-trip, the owner's stderr logs
/// `pipe.connect.rejected:not_allowed` (IR-0108 audit sink), and the
/// connector's stderr logs the owner's denial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "three live loopback processes; run with --ignored --test-threads=1"]
async fn unauthorized_member_pipe_denied() {
    let c = converge_three_member_room();
    let (echo_addr, echo_count) = spawn_echo_server().await;
    let echo_str = echo_addr.to_string();

    let expose = ChildSession::spawn(
        c.alice_home.path(),
        &[
            "pipe",
            "expose",
            &c.room,
            "--tcp",
            &echo_str,
            "--allow",
            &c.bob_id,
            "--loopback",
        ],
    );
    let pipe_line = expose
        .wait_for_line("pipe_id:", WAIT)
        .unwrap_or_else(|err| panic!("pipe expose never announced a pipe_id: {err}"));
    let pipe_id = extract_field(&pipe_line, "pipe_id")
        .expect("pipe expose must print a pipe_id")
        .to_owned();
    let expose_stdout = expose.stdout_snapshot();
    let listening = expose_stdout
        .lines()
        .find(|l| l.contains("listening:"))
        .expect("pipe expose must print a listening address");
    let alice_addr = parse_listening(listening);

    // The Agent (Active, not allow-listed) connects and drives traffic.
    let connect = ChildSession::spawn(
        c.agent_home.path(),
        &[
            "pipe",
            "connect",
            &c.room,
            &pipe_id,
            "--local",
            "0",
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    let forwarding = connect
        .wait_for_line("forwarding:", WAIT)
        .unwrap_or_else(|err| panic!("pipe connect never bound a local port: {err}"));
    let local = parse_forwarding(&forwarding);

    let mut client = TcpStream::connect(local).await.expect("connect local port");
    let _ = client.write_all(b"ping").await;
    let mut buf = [0u8; 4];
    let read = tokio::time::timeout(WAIT, client.read(&mut buf))
        .await
        .expect("read completes within budget");
    match read {
        Ok(0) | Err(_) => {} // clean EOF or reset — denied, no bytes forwarded
        Ok(n) => assert_ne!(
            &buf[..n],
            b"ping",
            "AC4: a denied stream must never echo forwarded bytes"
        ),
    }

    expose
        .wait_for_stderr_line("pipe.connect.rejected:not_allowed", WAIT)
        .expect("AC4: the owner must log the not_allowed rejection for the agent");
    connect
        .wait_for_stderr_line("[pipe] denied by the owner", WAIT)
        .expect("AC4: the agent must report the owner's denial");
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        0,
        "AC4: the owner must never connect to the echo target for the denied agent"
    );
}

/// The three converged homes plus the demo's shared file fixture, after the
/// full narrative has run and every session has been stopped. Shared by
/// `full_demo_two_humans_one_agent` and `full_demo_log_validates_after_restart`
/// (spec D5) so the ~12-step narrative is authored exactly once.
struct FullDemo {
    alice_home: TempDir,
    bob_home: TempDir,
    agent_home: TempDir,
    room: String,
}

/// Drive PRD §6's full ten-step demo end-to-end in causal order (spec §7.3),
/// asserting each step inline — the executable transcript. Returns the three
/// homes with every session stopped, so a caller can treat the return as a
/// genuine "all sessions down" restart point (§7.4).
#[allow(clippy::too_many_lines)] // one linear ten-step demo narrative; splitting fragments it
async fn run_full_demo_narrative() -> FullDemo {
    // §7.2 One-time setup.
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let agent_home = TempDir::new().expect("agent home");
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    identity_create(&agent_home, "build-agent");
    let bob_id = identity_id(&bob_home);
    let agent_id = identity_id(&agent_home);

    let room = room_create(&alice_home, "Full Demo Room");
    let bob_ticket = invite(&alice_home, &room, &bob_id, "member", Some("24h"));
    let agent_ticket = agent_invite(&alice_home, &room, &agent_id);
    assert!(
        bob_ticket.starts_with("roomtkt1") && agent_ticket.starts_with("roomtkt1"),
        "both invites are out-of-band capability tickets — no server involved"
    );

    // Alice shares the file OFFLINE, before any serving tail starts (D7: a
    // serving tail holds the blob store's exclusive lock).
    let content = b"two humans, one agent, zero servers";
    let src_dir = TempDir::new().expect("source dir");
    let src_path = write_file(src_dir.path(), "demo.txt", content);
    let share = one_shot(alice_home.path(), &["file", "share", &room, &src_path]);
    assert!(
        share.status.success(),
        "alice's file share must succeed; stderr: {}",
        String::from_utf8_lossy(&share.stderr)
    );
    let share_stdout = String::from_utf8_lossy(&share.stdout);
    let file_id = extract_field(&share_stdout, "file_id")
        .expect("file share must print a file_id")
        .to_owned();
    let declared_hash = extract_field(&share_stdout, "hash")
        .expect("file share must print a hash")
        .to_owned();

    // §7.3 step 5 — one serving Alice session: join-bootstrap + blob serving +
    // live receipt, all in the same `room tail --accept-joins` process.
    let alice_tail = ChildSession::spawn(
        alice_home.path(),
        &["room", "tail", &room, "--accept-joins", "--loopback"],
    );
    let listening = alice_tail
        .wait_for_line("listening:", WAIT)
        .unwrap_or_else(|err| panic!("alice tail never advertised a listening address: {err}"));
    let alice_addr = parse_listening(&listening);

    // step 6 — membership: Bob then the Agent join, converging to 3 active.
    let bob_join = one_shot(
        bob_home.path(),
        &[
            "room",
            "join",
            &bob_ticket,
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    assert!(
        bob_join.status.success(),
        "bob join must succeed; stderr: {}",
        String::from_utf8_lossy(&bob_join.stderr)
    );
    assert!(
        String::from_utf8_lossy(&bob_join.stdout).contains("members: 2 active"),
        "bob join must report a 2-member room"
    );

    let agent_join = one_shot(
        agent_home.path(),
        &[
            "room",
            "join",
            &agent_ticket,
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    assert!(
        agent_join.status.success(),
        "agent join must succeed; stderr: {}",
        String::from_utf8_lossy(&agent_join.stderr)
    );
    assert!(
        String::from_utf8_lossy(&agent_join.stdout).contains("members: 3 active"),
        "agent join must report a 3-member room"
    );
    wait_until_member_status(&alice_home, &room, &bob_id, "active", WAIT);
    wait_until_member_status(&alice_home, &room, &agent_id, "active", WAIT);

    // step 7 — signed message exchange: Bob sends, reaching Alice's live tail.
    let send = one_shot(
        bob_home.path(),
        &[
            "room",
            "send",
            &room,
            "prototype is up",
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    assert!(
        send.status.success(),
        "bob's send must succeed; stderr: {}",
        String::from_utf8_lossy(&send.stderr)
    );
    let send_stdout = String::from_utf8_lossy(&send.stdout);
    assert_eq!(extract_field(&send_stdout, "stored"), Some("yes"));
    assert!(
        send_stdout.contains("delivered: 1 connected peer(s)"),
        "bob's message must reach alice's live tail; got:\n{send_stdout}"
    );

    // step 8 — agent status (AC5 positive, online): delivered live, persists
    // durably on Alice.
    let status = one_shot(
        agent_home.path(),
        &[
            "agent",
            "status",
            &room,
            "running_tests",
            "--message",
            "suite in progress",
            "--progress",
            "40",
            "--peer",
            &alice_addr,
            "--loopback",
            "--timeout",
            "10s",
        ],
    );
    assert!(
        status.status.success(),
        "agent status must succeed; stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("delivered: 1 connected peer(s)"),
        "the agent's status push must reach alice's live tail"
    );

    // Both live touches above give every home at least one connection to
    // Alice, so the three-way roster now genuinely converges (recent-history
    // sync, IR-0201) — the natural point in the causal flow to assert it.
    let expected_roster: BTreeSet<(String, String, String)> = [
        (
            identity_id(&alice_home),
            "admin".to_owned(),
            "active".to_owned(),
        ),
        (bob_id.clone(), "member".to_owned(), "active".to_owned()),
        (agent_id.clone(), "agent".to_owned(), "active".to_owned()),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        roster_set(&members_json(&alice_home, &room)),
        expected_roster
    );
    assert_eq!(roster_set(&members_json(&bob_home, &room)), expected_roster);
    assert_eq!(
        roster_set(&members_json(&agent_home, &room)),
        expected_roster
    );

    let deadline = Instant::now() + WAIT;
    loop {
        let out = one_shot(
            alice_home.path(),
            &["room", "tail", &room, "--offline", "--json"],
        );
        assert!(out.status.success());
        let value: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("tail --json emits a JSON array");
        if value
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["event_type"] == "agent.status"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "alice never durably persisted the agent's pushed status within {WAIT:?}"
        );
        thread::sleep(Duration::from_millis(200));
    }

    // step 9 — file fetch + verify (AC3): both Bob and the Agent fetch (OQ-3),
    // proving the agent needs no special privilege to do so either.
    for (home, label) in [(&bob_home, "bob"), (&agent_home, "agent")] {
        let out_dir = TempDir::new().expect("fetch output dir");
        let out_dir_str = out_dir.path().to_string_lossy().into_owned();
        let fetch = one_shot(
            home.path(),
            &[
                "file",
                "fetch",
                &room,
                &file_id,
                "--out",
                &out_dir_str,
                "--peer",
                &alice_addr,
                "--loopback",
            ],
        );
        assert!(
            fetch.status.success(),
            "{label}'s file fetch must succeed; stderr: {}",
            String::from_utf8_lossy(&fetch.stderr)
        );
        let fetch_stdout = String::from_utf8_lossy(&fetch.stdout);
        let verified_hash = extract_field(&fetch_stdout, "verified")
            .expect("file fetch must print a verified hash")
            .to_owned();
        assert_eq!(
            verified_hash, declared_hash,
            "{label}: verified hash must equal the hash file share declared"
        );
        let saved_path = extract_field(&fetch_stdout, "saved")
            .expect("file fetch must print a saved path")
            .to_owned();
        let saved_bytes = std::fs::read(&saved_path).unwrap_or_else(|err| {
            panic!("{label}'s saved file at {saved_path} must be readable: {err}")
        });
        assert_eq!(
            saved_bytes, content,
            "{label}: saved bytes must equal the original shared content"
        );
    }

    // step 10 — stop Alice's serving session: frees the blob lock for the pipe leg.
    drop(alice_tail);

    // step 11 — live pipe (AC4): Alice exposes to Bob only.
    let (echo_addr, echo_count) = spawn_echo_server().await;
    let echo_str = echo_addr.to_string();
    let expose = ChildSession::spawn(
        alice_home.path(),
        &[
            "pipe",
            "expose",
            &room,
            "--tcp",
            &echo_str,
            "--allow",
            &bob_id,
            "--loopback",
        ],
    );
    let pipe_line = expose
        .wait_for_line("pipe_id:", WAIT)
        .unwrap_or_else(|err| panic!("pipe expose never announced a pipe_id: {err}"));
    let pipe_id = extract_field(&pipe_line, "pipe_id")
        .expect("pipe expose must print a pipe_id")
        .to_owned();
    let expose_stdout = expose.stdout_snapshot();
    let pipe_alice_addr = parse_listening(
        expose_stdout
            .lines()
            .find(|l| l.contains("listening:"))
            .expect("pipe expose must print a listening address"),
    );

    // Authorized: Bob connects and round-trips a `ping`.
    {
        let bob_connect = ChildSession::spawn(
            bob_home.path(),
            &[
                "pipe",
                "connect",
                &room,
                &pipe_id,
                "--local",
                "0",
                "--peer",
                &pipe_alice_addr,
                "--loopback",
            ],
        );
        let forwarding = bob_connect
            .wait_for_line("forwarding:", WAIT)
            .unwrap_or_else(|err| panic!("bob's pipe connect never bound a local port: {err}"));
        let local = parse_forwarding(&forwarding);
        let echoed = tokio::time::timeout(WAIT, async {
            let mut client = TcpStream::connect(local).await.expect("connect local port");
            client.write_all(b"ping").await.expect("write ping");
            let mut buf = [0u8; 4];
            client.read_exact(&mut buf).await.expect("read echo");
            buf
        })
        .await
        .expect("authorized round-trip within budget");
        assert_eq!(
            &echoed, b"ping",
            "AC4: bob's bytes must echo back through the pipe"
        );
        assert!(
            echo_count.load(Ordering::SeqCst) >= 1,
            "AC4: the owner must have connected to the echo target for bob"
        );
    }

    // Unauthorized: the Agent (Active, not allow-listed) is denied.
    {
        let agent_connect = ChildSession::spawn(
            agent_home.path(),
            &[
                "pipe",
                "connect",
                &room,
                &pipe_id,
                "--local",
                "0",
                "--peer",
                &pipe_alice_addr,
                "--loopback",
            ],
        );
        let forwarding = agent_connect
            .wait_for_line("forwarding:", WAIT)
            .unwrap_or_else(|err| panic!("agent's pipe connect never bound a local port: {err}"));
        let local = parse_forwarding(&forwarding);
        let mut client = TcpStream::connect(local)
            .await
            .expect("connect agent's local port");
        let _ = client.write_all(b"ping").await;
        let mut buf = [0u8; 4];
        let read = tokio::time::timeout(WAIT, client.read(&mut buf))
            .await
            .expect("read completes within budget");
        match read {
            Ok(0) | Err(_) => {}
            Ok(n) => assert_ne!(
                &buf[..n],
                b"ping",
                "AC4: a denied stream must never echo forwarded bytes"
            ),
        }
        expose
            .wait_for_stderr_line("pipe.connect.rejected:not_allowed", WAIT)
            .expect("AC4: the owner must log the not_allowed rejection for the agent");
        agent_connect
            .wait_for_stderr_line("[pipe] denied by the owner", WAIT)
            .expect("AC4: the agent must report the owner's denial");
    }
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        1,
        "AC4: only bob's authorized connection ever reached the echo target"
    );
    drop(expose);

    // step 12 — clean pipe close: leaves a matched open/close pair on the log.
    let close = one_shot(
        alice_home.path(),
        &["pipe", "close", &pipe_id, "--room", &room],
    );
    assert!(
        close.status.success(),
        "alice's pipe close must succeed; stderr: {}",
        String::from_utf8_lossy(&close.stderr)
    );

    FullDemo {
        alice_home,
        bob_home,
        agent_home,
        room,
    }
}

/// AC1 + AC3 + AC4 + agent-online — the whole PRD §6 demo, in causal order, as
/// one flow: three identities → room → two invites (human + agent) → one
/// serving Alice session → three-way join convergence → signed message →
/// live agent status → dual file fetch+verify → live pipe (authorized +
/// denied) → clean pipe close. Every step asserts inline, so a break
/// localizes to a step; this is the executable transcript (spec §8).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "three live loopback processes; run with --ignored --test-threads=1"]
async fn full_demo_two_humans_one_agent() {
    let _demo = run_full_demo_narrative().await;
}

/// Every event type wire-delivered to Alice by the online narrative: she
/// hosts the serving session, so she both authors and receives everything,
/// including the local-only `pipe.closed` (§7.3 step 12).
const ALICE_TYPES: [&str; 8] = [
    "room.created",
    "member.invited",
    "member.joined",
    "message.text",
    "file.shared",
    "pipe.opened",
    "pipe.closed",
    "agent.status",
];
/// Every event type wire-delivered to Bob/the Agent: identical to
/// [`ALICE_TYPES`] minus `pipe.closed`, which is authored by a one-shot local
/// `pipe close` after both peers' pipe sessions have already been torn down —
/// nobody is left online to push it to them (PRD §14: no guaranteed offline
/// delivery, not a bug).
const PEER_TYPES: [&str; 7] = [
    "room.created",
    "member.invited",
    "member.joined",
    "message.text",
    "file.shared",
    "pipe.opened",
    "agent.status",
];

/// AC2, online form — after the full networked demo has run and every
/// session has stopped, `room tail --offline --json` on each home re-validates
/// and re-folds byte-stably. Alice (host + author of every event) must show
/// every wire-delivered/authored type; Bob and the Agent must show every type
/// actually delivered to them (an admin-only local `pipe close` with nobody
/// left online to push to never reaches either — an honest MVP limitation,
/// not a bug: PRD §14 promises no guaranteed offline delivery).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "three live loopback processes; run with --ignored --test-threads=1"]
async fn full_demo_log_validates_after_restart() {
    let demo = run_full_demo_narrative().await;

    for (home, expected_types, label) in [
        (&demo.alice_home, ALICE_TYPES.as_slice(), "alice"),
        (&demo.bob_home, PEER_TYPES.as_slice(), "bob"),
        (&demo.agent_home, PEER_TYPES.as_slice(), "agent"),
    ] {
        let read1 = one_shot(
            home.path(),
            &["room", "tail", &demo.room, "--offline", "--json"],
        );
        assert!(
            read1.status.success(),
            "{label}'s offline tail --json must succeed; stderr: {}",
            String::from_utf8_lossy(&read1.stderr)
        );
        let rows: serde_json::Value =
            serde_json::from_slice(&read1.stdout).expect("tail --json emits a JSON array");
        let array = rows.as_array().expect("tail --json is an array");
        for expected_type in expected_types {
            assert!(
                array
                    .iter()
                    .any(|r| r["event_type"].as_str() == Some(*expected_type)),
                "{label}: expected event_type {expected_type:?} to be present after restart; rows: {rows}"
            );
        }

        let read2 = one_shot(
            home.path(),
            &["room", "tail", &demo.room, "--offline", "--json"],
        );
        assert!(
            read2.status.success(),
            "{label}'s second restart read must exit 0"
        );
        assert_eq!(
            read1.stdout, read2.stdout,
            "{label}: the offline JSON read must be byte-stable across restarts"
        );
    }

    // The three rosters still converge post-restart (no membership churn
    // occurred after the point they were shown converged in the narrative).
    let expected_roster: BTreeSet<(String, String, String)> = [
        (
            identity_id(&demo.alice_home),
            "admin".to_owned(),
            "active".to_owned(),
        ),
        (
            identity_id(&demo.bob_home),
            "member".to_owned(),
            "active".to_owned(),
        ),
        (
            identity_id(&demo.agent_home),
            "agent".to_owned(),
            "active".to_owned(),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        roster_set(&members_json(&demo.alice_home, &demo.room)),
        expected_roster
    );
    assert_eq!(
        roster_set(&members_json(&demo.bob_home, &demo.room)),
        expected_roster
    );
    assert_eq!(
        roster_set(&members_json(&demo.agent_home, &demo.room)),
        expected_roster
    );
}
