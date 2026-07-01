//! Two-peer product-slice integration test (IR-0109 / issue #24).
//!
//! This is the single integration test PRD §19 Phase 1A deliverable 8 names
//! explicitly: it chains the whole Phase 1A slice together and proves it works as a
//! product — two isolated participants, driven through the **real `iroh-rooms`
//! binary** over two isolated on-disk homes, converging on the same room and
//! exchanging a message and a live pipe **without a central application server**.
//!
//! ## Network stack
//!
//! Every online step runs with the hidden `--loopback` flag, which routes through
//! `NetMode::Loopback` (`presets::Minimal` + `RelayMode::Disabled`,
//! `crates/iroh-rooms-net/src/transport.rs`): no relay server, no n0 discovery —
//! pure loopback QUIC over `127.0.0.1`, wired together by parsing each host's
//! `listening:` address and threading it into the peer's `--peer`. That relay-free,
//! discovery-free stack is both what makes the online tier hermetic and the literal
//! proof of AC1 ("completes without central application server").
//!
//! ## Tiers (issue Test Plan: "Automated ... if reliable; otherwise gated local
//! test with documented command")
//!
//! | Issue AC | Test | Tier |
//! |---|---|---|
//! | AC1 — no central server | `full_slice_runs_without_central_server` | CI (`#[test]`) |
//! | AC2 — both peers agree on membership | `two_peers_converge_on_membership` | gated (`#[ignore]`) |
//! | AC3 — message persists across restart | `message_persists_across_restart` | CI (`#[test]`) |
//! | AC4 — pipe works for authorized peer | `authorized_pipe_forwards_bytes` | gated (`#[ignore]`) |
//! | AC5 — unauthorized connection denied | `unauthorized_pipe_connection_denied` | gated (`#[ignore]`) |
//!
//! The CI tier is deterministic and network-free (offline reads/writes over
//! `rooms.db`) so it can never flake `scripts/verify.sh`. The online tier needs two
//! live loopback processes to rendezvous, so it is `#[ignore]`-gated and run with the
//! documented command below. Every AC is *also* covered green-in-CI at the lower
//! Node-API layer by `iroh-rooms-net/tests/{join_e2e,message_e2e,pipe_e2e}.rs`, so
//! gating the CLI online tier loses no guaranteed coverage — it adds product-level
//! coverage on top.
//!
//! ## Running the gated tier
//!
//! ```bash
//! # Full two-peer product-slice proof (membership convergence + live pipe).
//! # Loopback only; no relay, no external tools. Serialize to avoid port contention.
//! cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1
//! ```
//!
//! No production code is modified: everything the test drives (`--loopback`,
//! `--peer`, `--accept-joins`, `room tail --offline --json`, `room members --json`,
//! the `pipe expose` stderr audit sink) already ships.

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

/// Per-network-step budget (matches the Node-API e2e suites). Every wait is
/// bounded by this so a rendezvous bug fails fast instead of hanging CI.
const WAIT: Duration = Duration::from_secs(15);

// ── binary + one-shot helpers ─────────────────────────────────────────────────

/// Resolve the built `iroh-rooms` binary once, so both `assert_cmd` one-shots and
/// the raw `std::process::Command` child sessions target the same artifact (R8).
fn bin_path() -> PathBuf {
    assert_cmd::cargo::cargo_bin("iroh-rooms")
}

/// Run a one-shot `iroh-rooms <args…>` against `dir`, capturing its output. Isolation
/// mirrors the other CLI suites: `IROH_ROOMS_HOME` is removed and `--data-dir` pins
/// the home explicitly.
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

/// The trimmed value that follows `key:` on a `key: value` line (ported verbatim
/// from `tests/message_cli.rs`).
fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

/// The `roomtkt1…` token printed on the indented line under `ticket:` (ported from
/// `tests/join_cli.rs`).
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

/// Parse the bound loopback socket out of a `forwarding: 127.0.0.1:<port> -> pipe
/// <id>` line (the first whitespace token after `forwarding:`).
fn parse_forwarding(line: &str) -> SocketAddr {
    let rest = extract_field(line, "forwarding").unwrap_or("");
    let token = rest.split_whitespace().next().unwrap_or("");
    token
        .parse()
        .unwrap_or_else(|_| panic!("could not parse forwarding socket from {line:?}"))
}

// ── fixture helpers (identity / room / invite / roster) ───────────────────────

/// `iroh-rooms identity create --name <name>` — must succeed.
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

/// Poll `room members --json` on `home` until `member_id` reads `active`, or panic
/// after `timeout` (absorbs the tail child's async persistence window; A2).
fn wait_until_member_active(home: &TempDir, room: &str, member_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let roster = members_json(home, room);
        let active = roster["members"].as_array().is_some_and(|members| {
            members.iter().any(|m| {
                m["identity_id"].as_str() == Some(member_id)
                    && m["status"].as_str() == Some("active")
            })
        });
        if active {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "member {member_id} did not become active within {timeout:?}; roster: {roster}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

// ── ChildSession: a spawned long-running `iroh-rooms` session ──────────────────

/// A spawned long-running `iroh-rooms` session (`room tail`, `pipe expose`,
/// `pipe connect`). Reader threads drain stdout/stderr into shared buffers so the
/// child never blocks on a full pipe; [`Drop`] kills the child so no orphan survives
/// a panic or early return.
struct ChildSession {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

/// Drain `pipe` line-by-line into `into` (each line kept with a trailing newline so
/// snapshots read naturally). Exits on EOF or a read error.
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
    /// Spawn `iroh-rooms <args…>` against `data_dir` with piped stdio and reader
    /// threads. `stdin` is closed so a child never blocks awaiting input.
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
    /// elapses. Used for the CLI-native pipe denial signals (AC5).
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
        // SIGKILL is the portable, unsafe-free stop (the workspace forbids `unsafe`,
        // so a `kill(2)` SIGTERM shim is unavailable). The temp homes are discarded
        // at test end, so a pipe lingering on the log until GC is irrelevant (R6).
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in std::mem::take(&mut self.readers) {
            let _ = reader.join();
        }
    }
}

// ── in-test loopback echo target (the pipe-tier "service being exposed") ──────

/// A loopback TCP echo server (the `--tcp` forward target for the pipe tier).
/// Returns its address and a counter of accepted connections, so a denial test can
/// prove the owner never connected to it (mirrors `pipe_e2e::spawn_echo_server`).
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

// ── convergence fixture (shared by AC2 / AC4 / AC5) ───────────────────────────

/// A converged two-member `{Alice, Bob}` room across two isolated homes.
struct Converged {
    alice_home: TempDir,
    bob_home: TempDir,
    room: String,
    alice_id: String,
    bob_id: String,
}

/// Drive the full membership handshake over loopback so both homes hold a
/// 2-member Active room: Alice hosts `room tail --accept-joins`, Bob redeems the
/// ticket with `room join --peer <alice>`, and Alice's roster is polled until Bob
/// reads active. Alice's tail session is stopped before returning.
fn converge_two_member_room() -> Converged {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    let alice_id = identity_id(&alice_home);
    let bob_id = identity_id(&bob_home);
    let room = room_create(&alice_home, "Two-Peer Room");
    let ticket = invite(&alice_home, &room, &bob_id, "member", Some("24h"));

    // Alice hosts the provisional join-bootstrap window and advertises her address.
    let alice_tail = ChildSession::spawn(
        alice_home.path(),
        &["room", "tail", &room, "--accept-joins", "--loopback"],
    );
    let listening = alice_tail
        .wait_for_line("listening:", WAIT)
        .unwrap_or_else(|err| panic!("alice tail never advertised a listening address: {err}"));
    let alice_addr = parse_listening(&listening);

    // Bob redeems the ticket, dialing Alice deterministically over loopback.
    let join = one_shot(
        bob_home.path(),
        &["room", "join", &ticket, "--peer", &alice_addr, "--loopback"],
    );
    assert!(
        join.status.success(),
        "bob join must succeed; stderr: {}",
        String::from_utf8_lossy(&join.stderr)
    );
    let join_stdout = String::from_utf8_lossy(&join.stdout);
    assert!(
        join_stdout.contains("members: 2 active"),
        "bob join must report a 2-member room; got:\n{join_stdout}"
    );

    // The join returns only after the admin observed it, but the tail child persists
    // asynchronously — poll Alice's roster to absorb that window.
    wait_until_member_active(&alice_home, &room, &bob_id, WAIT);
    drop(alice_tail);

    Converged {
        alice_home,
        bob_home,
        room,
        alice_id,
        bob_id,
    }
}

// ══ CI tier (deterministic, network-free) ═════════════════════════════════════

// ── Harness helper unit tests ─────────────────────────────────────────────────
//
// The helpers below are called by every online-tier test. A bug in any of them
// would silently corrupt the online-tier assertions, so they get their own
// deterministic, network-free coverage here.

/// `extract_field` returns the trimmed value that follows `key:` on a matching line.
#[test]
fn extract_field_finds_value_after_colon() {
    let output = "room_id: blake3:aabb\nstored: yes\n";
    assert_eq!(extract_field(output, "room_id"), Some("blake3:aabb"));
    assert_eq!(extract_field(output, "stored"), Some("yes"));
}

/// `extract_field` returns `None` for a key that is absent.
#[test]
fn extract_field_returns_none_for_absent_key() {
    let output = "foo: bar\n";
    assert_eq!(extract_field(output, "baz"), None);
}

/// `extract_ticket` finds the `roomtkt1…` token on the line that follows `ticket:`.
#[test]
fn extract_ticket_finds_roomtkt1_token() {
    let output = "invite_id: abc\nticket:\n  roomtkt1sometoken\nrole: member\n";
    assert_eq!(extract_ticket(output), Some("roomtkt1sometoken"));
}

/// `extract_ticket` returns `None` when no `ticket:` label is present.
#[test]
fn extract_ticket_returns_none_when_absent() {
    let output = "invite_id: abc\nrole: member\n";
    assert_eq!(extract_ticket(output), None);
}

/// `extract_ticket` returns `None` when `ticket:` is the very last line with no token
/// on the following line. The token must appear on the line immediately after `ticket:`.
#[test]
fn extract_ticket_returns_none_when_ticket_label_is_last_line() {
    let output = "invite_id: abc\nticket:";
    assert_eq!(
        extract_ticket(output),
        None,
        "extract_ticket must return None when ticket: has no following token line"
    );
}

/// `parse_listening` extracts everything after `listening:` (the full
/// `<ENDPOINT_ID>[@<ip:port>]` address passed to `--peer`).
#[test]
fn parse_listening_extracts_address() {
    let line = "listening: abcdef0123456789@127.0.0.1:54321";
    assert_eq!(parse_listening(line), "abcdef0123456789@127.0.0.1:54321");
}

/// `parse_forwarding` extracts the loopback socket from a
/// `forwarding: 127.0.0.1:<port> -> pipe <id>` line.
#[test]
fn parse_forwarding_extracts_socket_addr() {
    let line = "forwarding: 127.0.0.1:9999 -> pipe abcdef";
    let addr = parse_forwarding(line);
    assert_eq!(
        addr,
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 9999))
    );
}

/// `roster_set` must produce equal sets regardless of array order. AC2's convergence
/// assertion compares `roster_set(alice_roster) == roster_set(bob_roster)`; if the two
/// homes happen to persist their `members` arrays in different internal orders, the
/// sets must still compare equal.
#[test]
fn roster_set_is_order_independent() {
    // Same two members, different array positions.
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

// ── CI-tier integration: fixture helpers and offline convergence oracle ────────

/// The `identity_id` helper calls `identity show --json` and extracts the
/// `identity_id` field. The returned string must be a 64-character lowercase hex
/// string — the same format the online tier threads into `--invitee` and
/// `--allow`.
#[test]
fn identity_id_helper_returns_64_char_lowercase_hex() {
    let home = TempDir::new().unwrap();
    identity_create(&home, "Bob");
    let id = identity_id(&home);
    assert_eq!(
        id.len(),
        64,
        "identity_id must be 64 hex chars (32 bytes); got {id:?}"
    );
    assert!(
        id.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "identity_id must be lowercase hex; got {id:?}"
    );
}

/// Two isolated homes produce distinct `identity_id`s: key generation is properly
/// scoped to its `--data-dir`, so a joiner's id is never aliased to the host's.
#[test]
fn two_isolated_homes_produce_distinct_identity_ids() {
    let alice_home = TempDir::new().unwrap();
    let bob_home = TempDir::new().unwrap();
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    let alice_id = identity_id(&alice_home);
    let bob_id = identity_id(&bob_home);
    assert_ne!(
        alice_id, bob_id,
        "two isolated homes must produce distinct identity_ids"
    );
}

/// `members_json` returns a valid roster for a fresh room: it must have `admin`
/// and `members` fields, and `roster_set` must yield exactly one active admin
/// entry. This exercises the offline convergence oracle that `two_peers_converge_on_membership`
/// relies on (AC2 assertion shape, verified network-free).
#[test]
fn members_json_fresh_room_has_correct_structure_and_roster_set() {
    let home = TempDir::new().unwrap();
    identity_create(&home, "Alice");
    let alice_id = identity_id(&home);
    let room = room_create(&home, "Structure Room");

    let roster = members_json(&home, &room);
    assert_eq!(
        roster["admin"].as_str(),
        Some(alice_id.as_str()),
        "roster.admin must equal Alice's identity_id"
    );
    assert!(
        roster["members"].is_array(),
        "roster.members must be a JSON array"
    );
    let set = roster_set(&roster);
    assert_eq!(
        set.len(),
        1,
        "a fresh room has exactly one member (the admin); got: {set:?}"
    );
    assert!(
        set.contains(&(alice_id, "admin".to_owned(), "active".to_owned())),
        "the only member must be Alice as admin/active; set: {set:?}"
    );
}

/// After `room invite`, `members_json` shows the invitee as `invited`. This
/// exercises the same roster oracle that the online AC2 tier uses for convergence
/// comparison, at the invite-boundary before any join.
#[test]
fn members_json_shows_invitee_as_invited_after_invite() {
    let alice_home = TempDir::new().unwrap();
    let bob_home = TempDir::new().unwrap();
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    let bob_id = identity_id(&bob_home);
    let room = room_create(&alice_home, "Invite Room");

    invite(&alice_home, &room, &bob_id, "member", None);

    let roster = members_json(&alice_home, &room);
    let has_bob_invited = roster["members"].as_array().is_some_and(|members| {
        members.iter().any(|m| {
            m["identity_id"].as_str() == Some(bob_id.as_str())
                && m["status"].as_str() == Some("invited")
        })
    });
    assert!(
        has_bob_invited,
        "Bob must appear as 'invited' in the roster after an invite; roster: {roster}"
    );
}

/// Security: the `roomtkt1…` ticket produced with a real invitee `identity_id` must
/// not contain the raw secret seeds from Alice's `identity.secret`. The secret
/// travels only inside the encoded capability secret, never in plaintext.
/// Regression guard for the invite→join capability path in the two-peer slice.
#[test]
fn invite_ticket_for_real_invitee_does_not_contain_secret_seeds() {
    let alice_home = TempDir::new().unwrap();
    let bob_home = TempDir::new().unwrap();
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    let bob_id = identity_id(&bob_home);
    let room = room_create(&alice_home, "Secret Room");

    let ticket = invite(&alice_home, &room, &bob_id, "member", None);

    let secret_raw = std::fs::read_to_string(alice_home.path().join("identity.secret"))
        .expect("identity.secret must exist after identity create");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("identity.secret must be valid JSON");
    let identity_seed = secret_v["identity_secret"]
        .as_str()
        .expect("identity_secret field")
        .to_owned();
    let device_seed = secret_v["device_secret"]
        .as_str()
        .expect("device_secret field")
        .to_owned();

    assert!(
        !ticket.contains(&identity_seed),
        "ticket must not contain Alice's identity secret seed"
    );
    assert!(
        !ticket.contains(&device_seed),
        "ticket must not contain Alice's device secret seed"
    );
}

/// Harness self-guard: a `ChildSession` captures a spawned child's stdout, so a
/// harness regression is caught independently of the networked tiers.
#[test]
fn child_session_captures_output() {
    let home = TempDir::new().unwrap();
    let session = ChildSession::spawn(home.path(), &["identity", "create", "--name", "Harness"]);
    session
        .wait_for_line("identity_id:", WAIT)
        .expect("ChildSession must capture the child's stdout");
}

/// Harness self-guard for stderr: `ChildSession` must also drain the child's stderr
/// into the shared buffer. AC5 depends on `wait_for_stderr_line` finding
/// `pipe.connect.rejected:not_allowed` and `[pipe] denied by the owner` — if the
/// stderr drain is broken, those signals are silently lost.
#[test]
fn child_session_captures_stderr() {
    let home = TempDir::new().unwrap();
    // A syntactically malformed room id always produces an `invalid room id` error on
    // stderr and exits non-zero immediately — no identity or network needed.
    let session = ChildSession::spawn(
        home.path(),
        &["room", "tail", "not-a-valid-id", "--offline"],
    );
    session
        .wait_for_stderr_line("invalid", Duration::from_secs(5))
        .expect("ChildSession must capture the child's stderr for failing commands");
}

/// AC1 — the full offline backbone (identity → room → invite → offline send →
/// offline reads) succeeds with only local `--data-dir` stores and no server started
/// by the harness. The whole product slice is local-first: no relay, broker, or
/// application server is anywhere in the loop.
#[test]
fn full_slice_runs_without_central_server() {
    let alice = TempDir::new().unwrap();
    let bob = TempDir::new().unwrap();
    identity_create(&alice, "Alice");
    identity_create(&bob, "Bob");
    let alice_id = identity_id(&alice);
    let bob_id = identity_id(&bob);
    let room = room_create(&alice, "Local First Room");

    // The invite is the out-of-band capability path — minted locally, no server.
    let ticket = invite(&alice, &room, &bob_id, "member", None);
    assert!(
        ticket.starts_with("roomtkt1"),
        "invite must mint a roomtkt1… ticket; got: {ticket}"
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

/// AC3 — a message survives a restart. Write it with one `iroh-rooms` invocation,
/// then read it back with a **separate** process (`room tail --offline --json`) over
/// the same home: a real cold-store restart, network-free. The read is also asserted
/// byte-stable across two invocations (fold determinism at the binary boundary).
#[test]
fn message_persists_across_restart() {
    let home = TempDir::new().unwrap();
    identity_create(&home, "Alice");
    let room = room_create(&home, "Persist Room");
    let body = "persisted across a restart";

    let send = one_shot(home.path(), &["room", "send", &room, body]);
    assert!(
        send.status.success(),
        "offline send must exit 0; stderr: {}",
        String::from_utf8_lossy(&send.stderr)
    );
    let send_out = String::from_utf8_lossy(&send.stdout);
    assert_eq!(
        extract_field(&send_out, "stored"),
        Some("yes"),
        "the message must be stored locally"
    );

    // Fresh process, cold store: the offline JSON read must contain the message body.
    let read1 = one_shot(home.path(), &["room", "tail", &room, "--offline", "--json"]);
    assert!(
        read1.status.success(),
        "restart read must exit 0; stderr: {}",
        String::from_utf8_lossy(&read1.stderr)
    );
    let stdout1 = String::from_utf8_lossy(&read1.stdout);
    let rows: serde_json::Value =
        serde_json::from_str(stdout1.trim()).expect("offline tail --json must be a JSON array");
    let found = rows
        .as_array()
        .expect("tail --json is an array")
        .iter()
        .any(|row| {
            row["event_type"].as_str() == Some("message.text") && row["body"].as_str() == Some(body)
        });
    assert!(
        found,
        "the restarted read must contain the persisted message.text body; rows: {rows}"
    );

    // Byte-stability: a second cold read reproduces the same bytes exactly.
    let read2 = one_shot(home.path(), &["room", "tail", &room, "--offline", "--json"]);
    assert!(read2.status.success(), "second restart read must exit 0");
    assert_eq!(
        read1.stdout, read2.stdout,
        "the offline JSON read must be byte-stable across restarts"
    );
}

/// AC3 strengthened — three messages sent in sequence must all survive a cold-store
/// restart and appear in ascending lamport order in `room tail --offline --json`.
/// (`message_persists_across_restart` guards one message; this regression guards
/// completeness and ordering when multiple messages are present.)
#[test]
fn multiple_messages_persist_across_restart_in_order() {
    let home = TempDir::new().unwrap();
    identity_create(&home, "Alice");
    let room = room_create(&home, "Multi-Persist Room");
    let bodies = ["first persisted", "second persisted", "third persisted"];

    for body in bodies {
        let send = one_shot(home.path(), &["room", "send", &room, body]);
        assert!(
            send.status.success(),
            "offline send of {body:?} must exit 0; stderr: {}",
            String::from_utf8_lossy(&send.stderr)
        );
    }

    // Fresh process, cold store.
    let read = one_shot(home.path(), &["room", "tail", &room, "--offline", "--json"]);
    assert!(
        read.status.success(),
        "multi-message restart read must exit 0; stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    let stdout = String::from_utf8_lossy(&read.stdout);
    let all_rows: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("offline tail --json must be a valid JSON array");

    // Every expected body must be present.
    for body in bodies {
        assert!(
            all_rows
                .as_array()
                .expect("tail --json is a JSON array")
                .iter()
                .any(|r| r["event_type"].as_str() == Some("message.text")
                    && r["body"].as_str() == Some(body)),
            "message {body:?} must survive the restart"
        );
    }

    // Lamport positions of message rows must be strictly ascending (fold is deterministic).
    let lamports: Vec<u64> = all_rows
        .as_array()
        .expect("tail --json is a JSON array")
        .iter()
        .filter_map(|r| {
            if r["event_type"].as_str() == Some("message.text") {
                r["lamport"].as_u64()
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        lamports.len(),
        bodies.len(),
        "exactly {} message lamport entries must be present; got {lamports:?}",
        bodies.len()
    );
    let mut sorted = lamports.clone();
    sorted.sort_unstable();
    assert_eq!(
        lamports, sorted,
        "messages must appear in ascending lamport order after restart; lamports: {lamports:?}"
    );
}

// ══ Online tier (two live loopback processes; #[ignore]-gated) ════════════════

/// AC2 — both peers agree on room membership. After Bob joins over loopback, both
/// Alice's and Bob's `room members --json` rosters must be set-equal: admin = Alice,
/// members = {Alice admin/active, Bob member/active}.
#[test]
#[ignore = "two live loopback processes; run with --ignored --test-threads=1"]
fn two_peers_converge_on_membership() {
    let c = converge_two_member_room();

    let alice_roster = members_json(&c.alice_home, &c.room);
    let bob_roster = members_json(&c.bob_home, &c.room);

    assert_eq!(
        alice_roster["admin"], bob_roster["admin"],
        "both homes must agree on the admin"
    );
    assert_eq!(
        alice_roster["admin"].as_str(),
        Some(c.alice_id.as_str()),
        "the agreed admin must be Alice"
    );

    let expected: BTreeSet<(String, String, String)> = [
        (c.alice_id.clone(), "admin".to_owned(), "active".to_owned()),
        (c.bob_id.clone(), "member".to_owned(), "active".to_owned()),
    ]
    .into_iter()
    .collect();
    let alice_set = roster_set(&alice_roster);
    let bob_set = roster_set(&bob_roster);
    assert_eq!(
        alice_set, bob_set,
        "both homes must agree on the membership set"
    );
    assert_eq!(
        alice_set, expected,
        "the converged roster must be {{Alice admin/active, Bob member/active}}"
    );
}

/// AC4 — the pipe forwards bytes for an authorized peer. In the converged room Alice
/// exposes a loopback echo target allowing Bob; Bob connects and a `ping` round-trips
/// through the pipe back to the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "two live loopback processes; run with --ignored --test-threads=1"]
async fn authorized_pipe_forwards_bytes() {
    let c = converge_two_member_room();
    let (echo_addr, echo_count) = spawn_echo_server().await;
    let echo_str = echo_addr.to_string();

    // Alice exposes the echo target, allowing Bob.
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

    // Bob connects through the pipe on an OS-assigned local port.
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

    // A `ping` written to Bob's local port must echo back through the pipe.
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
        "AC4: the owner must have connected to the echo target"
    );
}

/// AC5 — an unauthorized connection is denied. In the same converged room Alice
/// exposes allowing an id **other than Bob** (her own). Bob is an Active member but
/// not allow-listed, so his connect passes the CLI's active-member pre-check yet is
/// denied owner-side. The proof: no bytes round-trip, the owner's stderr logs
/// `pipe.connect.rejected:not_allowed`, and the connector's stderr logs the owner
/// denial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "two live loopback processes; run with --ignored --test-threads=1"]
async fn unauthorized_pipe_connection_denied() {
    let c = converge_two_member_room();
    let (echo_addr, echo_count) = spawn_echo_server().await;
    let echo_str = echo_addr.to_string();

    // Alice allows only herself — Bob is Active but not on the allow-list.
    let expose = ChildSession::spawn(
        c.alice_home.path(),
        &[
            "pipe",
            "expose",
            &c.room,
            "--tcp",
            &echo_str,
            "--allow",
            &c.alice_id,
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

    // Bob (Active, not allowed) connects and drives traffic through the pipe.
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

    // The denied stream must never echo `ping` — either EOF/reset or nothing.
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
            "AC5: a denied stream must never echo forwarded bytes"
        ),
    }

    // Both CLI-native denial signals must appear on stderr.
    expose
        .wait_for_stderr_line("pipe.connect.rejected:not_allowed", WAIT)
        .expect("AC5: the owner must log the not_allowed rejection (IR-0108 audit sink)");
    connect
        .wait_for_stderr_line("[pipe] denied by the owner", WAIT)
        .expect("AC5: the connector must report the owner's denial");
    assert_eq!(
        echo_count.load(Ordering::SeqCst),
        0,
        "AC5: the owner must never connect to the echo target for a denied member"
    );
}
