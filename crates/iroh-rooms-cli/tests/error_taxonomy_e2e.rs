//! End-to-end coverage for the CLI error taxonomy's network-boundary
//! connectivity codes (spec IR-0110 / issue #25).
//!
//! `tests/error_taxonomy.rs` is deliberately network-free; its own module doc
//! defers the connectivity codes to "the e2e phase" because they need a real
//! dial or a live two-peer session. This suite covers the ones that are
//! deterministic enough to automate:
//!
//! | Spec §8 test item | Test | Tier |
//! |---|---|---|
//! | #12 `no_admin_reachable` | `join_to_an_unreachable_peer_exits_6_no_admin_reachable` | CI (`#[test]`) |
//! | #8 `peer_unauthorized` (pipe close) | `pipe_close_by_a_non_owner_non_admin_member_exits_3_peer_unauthorized` | CI (`#[test]`) |
//! | #8 `peer_offline` (pipe connect) | `pipe_connect_to_an_offline_owner_exits_6_peer_offline` | gated (`#[ignore]`) |
//!
//! The `no_admin_reachable` case needs only one process: a real (never-answered)
//! loopback UDP dial to an admin who never brings a node online, so the timeout
//! is genuine rather than simulated, and it stays fast and deterministic enough
//! to run on every `cargo test` (no `--ignored` gate, matching the CI tier of
//! `two_peer_e2e.rs`). The `peer_unauthorized` case needs one real join to
//! converge a genuinely active, non-admin member, then asserts entirely against
//! local fold/pipe-metadata state (`pipe::close`'s `!is_admin && !is_owner`
//! guard) with no pipe or second live process required, so it also stays in the
//! ungated CI tier. The `peer_offline` case needs two live processes (an owner
//! that goes offline mid-session) and is gated the same way that file gates its
//! online tier.
//!
//! ## Intentionally out of scope
//!
//! `bad_signature`/`not_a_member` receive-path warnings (spec §8 test item #6)
//! and the `clock_skew` advisory (test item #13) are not covered here:
//! reproducing them faithfully needs a raw, hand-crafted wire frame (a
//! corrupted signature or a foreign sender) or a skewed system clock injected
//! under the real `iroh-rooms` binary — neither of which the public CLI surface
//! can drive without reimplementing the wire protocol inside the test (the kind
//! of harness `iroh-rooms-net/tests/frame.rs` builds at the transport-codec
//! layer, not the CLI layer). The underlying reject/flag behaviour is already
//! proven at the Node-API layer by `iroh-rooms-net/tests/message_e2e.rs`; the
//! CLI's rendering of it is a documented coverage gap (see the issue #25
//! report), not a silently-skipped requirement.
//!
//! ## Running the gated tier
//!
//! ```bash
//! cargo test -p iroh-rooms-cli --test error_taxonomy_e2e -- --ignored --test-threads=1
//! ```

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Per-network-step budget (matches `two_peer_e2e.rs`).
const WAIT: Duration = Duration::from_secs(15);

// ── binary + one-shot helpers (ported from `two_peer_e2e.rs`) ─────────────────

fn bin_path() -> PathBuf {
    assert_cmd::cargo::cargo_bin("iroh-rooms")
}

/// Run a one-shot `iroh-rooms <args…>` against `dir`, capturing its output.
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
/// address.
fn parse_listening(line: &str) -> String {
    extract_field(line, "listening").unwrap_or("").to_owned()
}

// ── fixture helpers (identity / room / invite) ────────────────────────────────

fn identity_create(home: &TempDir, name: &str) {
    let out = one_shot(home.path(), &["identity", "create", "--name", name]);
    assert!(
        out.status.success(),
        "identity create must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One field (`identity_id` or `device_id`) from `identity show --json`.
fn identity_field(home: &TempDir, key: &str) -> String {
    let out = one_shot(home.path(), &["identity", "show", "--json"]);
    assert!(
        out.status.success(),
        "identity show --json must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("identity show --json must be valid JSON");
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("identity show --json must carry a {key:?} field; got: {value}"))
        .to_owned()
}

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

fn invite(home: &TempDir, room: &str, invitee_id: &str, role: &str) -> String {
    let out = one_shot(
        home.path(),
        &[
            "room",
            "invite",
            room,
            "--invitee",
            invitee_id,
            "--role",
            role,
        ],
    );
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

// ── ChildSession: a spawned long-running `iroh-rooms` session ──────────────────
//
// Ported verbatim (behaviourally) from `two_peer_e2e.rs`: reader threads drain
// stdout/stderr so the child never blocks on a full pipe, and `Drop` kills the
// child so no orphan survives a panic or early return.

struct ChildSession {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

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

    fn scan(buf: &Arc<Mutex<String>>, needle: &str) -> Option<String> {
        buf.lock()
            .expect("capture buffer not poisoned")
            .lines()
            .find(|l| l.contains(needle))
            .map(str::to_owned)
    }

    fn wait_for_line(&self, needle: &str, timeout: Duration) -> Result<String, String> {
        self.wait_in(&self.stdout, "stdout", needle, timeout)
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
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in std::mem::take(&mut self.readers) {
            let _ = reader.join();
        }
    }
}

// ── convergence fixture (ported from `two_peer_e2e.rs`) ───────────────────────

/// A converged two-member `{Alice, Bob}` room across two isolated homes.
struct Converged {
    alice_home: TempDir,
    bob_home: TempDir,
    room: String,
    bob_id: String,
}

fn converge_two_member_room() -> Converged {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    let bob_id = identity_field(&bob_home, "identity_id");
    let room = room_create(&alice_home, "Two-Peer Taxonomy Room");
    let ticket = invite(&alice_home, &room, &bob_id, "member");

    let alice_tail = ChildSession::spawn(
        alice_home.path(),
        &["room", "tail", &room, "--accept-joins", "--loopback"],
    );
    let listening = alice_tail
        .wait_for_line("listening:", WAIT)
        .unwrap_or_else(|err| panic!("alice tail never advertised a listening address: {err}"));
    let alice_addr = parse_listening(&listening);

    let join = one_shot(
        bob_home.path(),
        &["room", "join", &ticket, "--peer", &alice_addr, "--loopback"],
    );
    assert!(
        join.status.success(),
        "bob join must succeed; stderr: {}",
        String::from_utf8_lossy(&join.stderr)
    );
    drop(alice_tail);

    Converged {
        alice_home,
        bob_home,
        room,
        bob_id,
    }
}

/// Poll `pipe list <room>` (offline, no live connection) on `home` until
/// `pipe_hex` appears, or panic after `timeout`. This confirms the
/// `pipe.opened` event is durably persisted in the local store — the
/// precondition for a later owner-offline connect to resolve `pipe_opened`
/// from disk alone, with no live sync involved.
fn wait_until_pipe_listed(home: &TempDir, room: &str, pipe_hex: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let needle = format!("pipe_id: {pipe_hex}");
    loop {
        let out = one_shot(home.path(), &["pipe", "list", room]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains(&needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "pipe {pipe_hex} never appeared in {room}'s local pipe list within {timeout:?}; \
             last stdout:\n{stdout}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

// ══ CI tier: no_admin_reachable (one process, a real never-answered dial) ═════

/// Spec §8 test item #12 / AC2 (connectivity, "join can't reach admin"): a
/// `room join` whose only dial target is a real (but never-live) admin device
/// at an unbound loopback port must fail closed with `no_admin_reachable`
/// (Connectivity, exit 6) — distinct from an authorization rejection or a
/// ticket decode failure. Alice never brings a node online, so the dial is a
/// genuine, unanswered network attempt rather than a simulated timeout, and
/// the whole test completes in about one `--timeout` window.
#[test]
fn join_to_an_unreachable_peer_exits_6_no_admin_reachable() {
    let home_admin = TempDir::new().unwrap();
    identity_create(&home_admin, "Alice");
    let alice_device_id = identity_field(&home_admin, "device_id");
    let room = room_create(&home_admin, "Unreachable Admin Room");

    let home_bob = TempDir::new().unwrap();
    identity_create(&home_bob, "Bob");
    let bob_id = identity_field(&home_bob, "identity_id");

    // The ticket is minted purely offline; Alice's node is never started.
    let ticket = invite(&home_admin, &room, &bob_id, "member");

    // A real endpoint id (Alice's actual device key), but at a loopback port
    // nobody listens on — a genuine dial that will never be answered.
    let dead_peer = format!("{alice_device_id}@127.0.0.1:1");
    let out = one_shot(
        home_bob.path(),
        &[
            "room",
            "join",
            &ticket,
            "--peer",
            &dead_peer,
            "--timeout",
            "1s",
            "--loopback",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(6),
        "a join whose admin never answers must exit 6 (Connectivity); stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[no_admin_reachable]:"),
        "must render the no_admin_reachable coded line; stderr: {stderr}"
    );
}

// ══ CI tier: peer_unauthorized (one real join; offline thereafter) ═══════════

/// Spec §8 test item #8 / AC2 (connectivity command twin, "pipe close by a
/// non-owner/non-admin"): a genuinely converged, active room member who is
/// neither the room admin nor a pipe's owner must be refused `pipe close` with
/// `peer_unauthorized` (Auth, exit 3) — distinct from the `peer_offline`
/// connectivity twin below. The refusal (`pipe.rs::close`'s
/// `!is_admin && !is_owner` guard) is checked entirely against the local fold
/// and a local pipe-metadata lookup, so once Bob has converged (the one live
/// join below) the assertion itself needs no live process and no pipe to
/// actually exist: `open_pipe` resolving to `None` for an arbitrary id makes
/// `is_owner` `false` exactly as a foreign pipe would.
#[test]
fn pipe_close_by_a_non_owner_non_admin_member_exits_3_peer_unauthorized() {
    let c = converge_two_member_room();

    // Bob is a real, active member post-join (not admin — Alice is), and owns no
    // pipe with this arbitrary id (none was ever opened). `--room` bypasses the
    // pipe-id-based room inference, so no pipe needs to exist for the check to
    // reach the `is_admin`/`is_owner` guard.
    let arbitrary_pipe_id = "0".repeat(32);
    let out = one_shot(
        c.bob_home.path(),
        &["pipe", "close", &arbitrary_pipe_id, "--room", &c.room],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a non-owner, non-admin pipe close must exit 3 (Auth); stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[peer_unauthorized]:"),
        "must render the peer_unauthorized coded line; stderr: {stderr}"
    );
}

// ══ Online tier (two live loopback processes; #[ignore]-gated) ════════════════

/// Spec §8 test item #8 / AC2 (connectivity, "pipe owner offline"): a
/// `pipe connect` whose `pipe.opened` is already synced locally, but whose
/// owner has since gone offline, must fail closed with `peer_offline`
/// (Connectivity, exit 6) — the command-failure twin of the `PeerConnState`
/// distinction `room members --status` renders live.
///
/// Sequenced in two phases so the assertion is deterministic rather than a
/// race: (1) both peers live, Bob syncs the pipe's announcement into his own
/// store via a brief `room tail` session, confirmed offline via `pipe list`;
/// (2) both live sessions torn down, then a **fresh** `pipe connect` resolves
/// `pipe_opened` from Bob's local store alone (no live sync needed) and only
/// then discovers the owner is unreachable.
#[test]
#[ignore = "two live loopback processes; run with --ignored --test-threads=1"]
fn pipe_connect_to_an_offline_owner_exits_6_peer_offline() {
    let c = converge_two_member_room();

    // Phase 1: Alice exposes a pipe allowing Bob; Bob syncs it, then disconnects.
    let expose = ChildSession::spawn(
        c.alice_home.path(),
        &[
            "pipe",
            "expose",
            &c.room,
            "--tcp",
            "127.0.0.1:9",
            "--allow",
            &c.bob_id,
            "--loopback",
        ],
    );
    let pipe_line = expose
        .wait_for_line("pipe_id:", WAIT)
        .unwrap_or_else(|err| panic!("pipe expose never announced a pipe_id: {err}"));
    let pipe_hex = extract_field(&pipe_line, "pipe_id")
        .expect("pipe expose must print a pipe_id")
        .to_owned();
    let expose_stdout = expose.stdout_snapshot();
    let listening = expose_stdout
        .lines()
        .find(|l| l.contains("listening:"))
        .expect("pipe expose must print a listening address");
    let alice_addr = parse_listening(listening);

    let bob_sync = ChildSession::spawn(
        c.bob_home.path(),
        &["room", "tail", &c.room, "--peer", &alice_addr, "--loopback"],
    );
    wait_until_pipe_listed(&c.bob_home, &c.room, &pipe_hex, WAIT);
    drop(bob_sync);

    // Phase 2: Alice goes offline entirely.
    drop(expose);

    // A fresh `pipe connect` resolves the already-persisted pipe.opened from
    // disk, then fails to dial the now-dead owner.
    let out = one_shot(
        c.bob_home.path(),
        &[
            "pipe",
            "connect",
            &c.room,
            &pipe_hex,
            "--local",
            "0",
            "--peer",
            &alice_addr,
            "--loopback",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(6),
        "pipe connect to an offline owner must exit 6 (Connectivity); stderr: {stderr}"
    );
    assert!(
        stderr.contains("error[peer_offline]:"),
        "must render the peer_offline coded line; stderr: {stderr}"
    );
}
