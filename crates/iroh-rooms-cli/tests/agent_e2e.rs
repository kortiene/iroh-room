//! Online two-peer e2e coverage for agent identity (IR-0206 / issue #31).
//!
//! `crates/iroh-rooms-cli/tests/agent_cli.rs` proves the four ACs offline
//! (invite → `status: invited`), and its module doc explicitly defers the
//! online **join** half — an agent redeeming its ticket and converging to
//! `role: agent, status: active` on both peers — to "the e2e phase", following
//! the `two_peer_e2e.rs` `#[ignore]` loopback convention. This file is that
//! deferred coverage.
//!
//! ## Network stack
//!
//! Identical to `two_peer_e2e.rs`: the hidden `--loopback` flag routes through
//! `NetMode::Loopback` (no relay, no discovery) — pure loopback QUIC over
//! `127.0.0.1`, with each host's `listening:` address threaded into the peer's
//! `--peer`. No central application server is anywhere in the loop.
//!
//! ## Tiers
//!
//! | Coverage | Test | Tier |
//! |---|---|---|
//! | AC2 (agent join converges, both peers agree) | `agent_joins_and_converges_with_agent_role` | gated (`#[ignore]`) |
//!
//! This is network-dependent (two live loopback processes must rendezvous), so
//! it follows the same `#[ignore]`-gated tier as the rest of the online
//! two-peer suite rather than the always-green CI tier.
//!
//! AC3 ("no implicit access") is deliberately **not** re-tested here at the
//! wire level: `join::join` pre-checks the ticket's key binding before any
//! dial (see `join.rs`), so a mismatched-identity join never touches the
//! network and is already covered, network-free, by
//! `join_cli.rs::join_wrong_identity_exits_nonzero_with_actionable_message`, and,
//! through the *agent* invite surface specifically, by IR-0207's
//! `agent_invite_flow.rs` (corrupt / truncated / wrong-identity agent tickets).
//! The network-layer admission gate is role-agnostic (it keys on
//! identity/device, not on `role`) and already has dedicated online coverage in
//! `iroh-rooms-net/tests/manager_e2e.rs::managed_room_unknown_inbound_rejected_by_snapshot_admission`;
//! duplicating it for an `agent`-flavored device would add no new guarantee.
//!
//! The remaining online leg of AC3 — a *structurally valid* agent ticket rejected
//! by a live admin's `gate_join` (wrong capability secret / expired invite) — is
//! also not here: it is proven at the `Node` layer, `role = "agent"`, by
//! `iroh-rooms-net/tests/join_e2e.rs::agent_bad_capability_secret_join_not_accepted`
//! and `…::agent_expired_invite_join_not_accepted`, which mirror the member-role
//! proofs in the same file (always-green — no live process/loopback socket
//! needed, just two in-process `Node`s).
//!
//! ## Running the gated tier
//!
//! ```bash
//! cargo test -p iroh-rooms-cli --test agent_e2e -- --ignored --test-threads=1
//! ```
//!
//! No production code is modified: everything driven here (`agent invite`,
//! `room tail --accept-joins --loopback`, `room join --peer --loopback`, `room
//! members --json`) already ships.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Per-network-step budget (matches `two_peer_e2e.rs`).
const WAIT: Duration = Duration::from_secs(15);

// ── binary + one-shot helpers (ported from two_peer_e2e.rs) ───────────────────

fn bin_path() -> PathBuf {
    assert_cmd::cargo::cargo_bin("iroh-rooms")
}

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

fn extract_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            return Some(rest.strip_prefix(':').unwrap_or(rest).trim());
        }
    }
    None
}

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

// ── fixture helpers (identity / room / agent invite / roster) ─────────────────

fn identity_create(home: &TempDir, name: &str) {
    let out = one_shot(home.path(), &["identity", "create", "--name", name]);
    assert!(
        out.status.success(),
        "identity create must succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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

/// `iroh-rooms agent invite <room> <agent_id> [--expires <e>]` → the `roomtkt1…`
/// ticket. Exercises the `agent` noun itself (not `room invite --role agent`),
/// since this suite's purpose is proving the *documented* agent surface joins
/// end-to-end.
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

/// The `{identity_id, role, status}` triples of a roster, order-independent.
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
/// panic after `timeout`.
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

// ── ChildSession: a spawned long-running `iroh-rooms` session (ported) ────────

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
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = Self::scan(&self.stdout, needle) {
                return Ok(line);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {needle:?} on stdout\n\
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

// ══ Online tier (two live loopback processes; #[ignore]-gated) ════════════════

/// AC2 (online half): an admin invites a known agent identity via `agent invite`
/// (the documented IR-0206 noun, not `room invite --role agent`); the agent
/// redeems the ticket over real loopback QUIC with `room join --peer --loopback`.
/// After convergence, **both** the admin's and the agent's own `room members
/// --json` rosters agree: the agent reads `role: agent, status: active`, exactly
/// mirroring `two_peer_e2e.rs::two_peers_converge_on_membership` but for the
/// `agent` role and the `agent invite` CLI surface — closing the online half the
/// offline `agent_cli.rs` suite explicitly defers to this e2e phase.
#[test]
#[ignore = "two live loopback processes; run with --ignored --test-threads=1"]
fn agent_joins_and_converges_with_agent_role() {
    let admin_home = TempDir::new().expect("admin home");
    let agent_home = TempDir::new().expect("agent home");
    identity_create(&admin_home, "Alice");
    identity_create(&agent_home, "build-agent");
    let admin_id = identity_id(&admin_home);
    let agent_id = identity_id(&agent_home);
    let room = room_create(&admin_home, "Agent E2E Room");

    let ticket = agent_invite(&admin_home, &room, &agent_id);

    // Admin hosts the provisional join-bootstrap window and advertises her address.
    let admin_tail = ChildSession::spawn(
        admin_home.path(),
        &["room", "tail", &room, "--accept-joins", "--loopback"],
    );
    let listening = admin_tail
        .wait_for_line("listening:", WAIT)
        .unwrap_or_else(|err| panic!("admin tail never advertised a listening address: {err}"));
    let admin_addr = parse_listening(&listening);

    // The agent redeems its ticket, dialing the admin deterministically over loopback.
    let join = one_shot(
        agent_home.path(),
        &["room", "join", &ticket, "--peer", &admin_addr, "--loopback"],
    );
    assert!(
        join.status.success(),
        "agent join must succeed; stderr: {}",
        String::from_utf8_lossy(&join.stderr)
    );
    let join_stdout = String::from_utf8_lossy(&join.stdout);
    assert!(
        join_stdout.contains("members: 2 active"),
        "agent join must report a 2-member room; got:\n{join_stdout}"
    );

    // The join returns only after the admin observed it, but the tail child
    // persists asynchronously — poll the admin's roster to absorb that window.
    wait_until_member_status(&admin_home, &room, &agent_id, "active", WAIT);
    drop(admin_tail);

    let admin_roster = members_json(&admin_home, &room);
    let agent_roster = members_json(&agent_home, &room);

    assert_eq!(
        admin_roster["admin"], agent_roster["admin"],
        "both homes must agree on the admin"
    );

    let expected: BTreeSet<(String, String, String)> = [
        (admin_id.clone(), "admin".to_owned(), "active".to_owned()),
        (agent_id.clone(), "agent".to_owned(), "active".to_owned()),
    ]
    .into_iter()
    .collect();
    let admin_set = roster_set(&admin_roster);
    let agent_set = roster_set(&agent_roster);
    assert_eq!(
        admin_set, agent_set,
        "both peers must converge on the same membership set (AC2)"
    );
    assert_eq!(
        admin_set, expected,
        "the converged roster must be {{admin admin/active, agent agent/active}} — \
         the agent role survives the wire round-trip, not just the local fold"
    );
}
