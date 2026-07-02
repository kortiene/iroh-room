//! End-to-end coverage for IR-0303's (issue #38) `--verbose` network-diagnostics
//! surface: the live `diag:` block on `room members --status --verbose`.
//!
//! `tests/error_taxonomy.rs::members_verbose_requires_status` /
//! `room_tail_accepts_verbose_and_parses_room_id_first` prove the flag is
//! *wired* (clap-level, network-free) — their own doc comments explicitly defer
//! what it actually *renders* to "the e2e phase", because a `diag: peer …` line
//! only exists once a second, real peer is live to classify a path against
//! (`Node::peer_paths` reads `Endpoint::remote_info`, spec §5.3). This suite is
//! that phase, covering spec §8 test items 5-7:
//!
//! | spec §8 item | test |
//! |---|---|
//! | #5 diag block present (valid `path=` label); hidden by default | `members_status_verbose_renders_diag_block_default_hides_it` |
//! | #6 verbose is stderr-only (stdout script-clean) | `members_status_verbose_diag_lines_are_stderr_only` |
//! | #7 AC3 no secret leak (identity/device seed) | `members_status_verbose_leaks_no_identity_or_device_seed` |
//!
//! `path=` is asserted to be *one of* `direct`/`relay`/`mixed`/`none` rather than
//! a specific value: spec §5.3's "settle nuance" documents that `remote_info` can
//! honestly read `None` (⇒ `path=none`) before a loopback link's active address
//! set has settled, even after `PeerConnState` already reads `connected` — so
//! pinning a specific label would be flaky by construction, not a real bug.
//!
//! ## Network stack
//! Every step runs `--loopback` (relay-free, discovery-free loopback QUIC),
//! mirroring `two_peer_e2e.rs` / `error_taxonomy_e2e.rs`.
//!
//! ## Running
//! CI tier (no live-process teardown/redial beyond the initial join-then-dial
//! shape already proven CI-safe by `error_taxonomy_e2e.rs`'s
//! `converge_two_member_room`):
//! ```bash
//! cargo test -p iroh-rooms-cli --test diagnostics_cli
//! ```

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Per-network-step budget (matches `two_peer_e2e.rs` / `error_taxonomy_e2e.rs`).
const WAIT: Duration = Duration::from_secs(15);

// ── binary + one-shot helpers (ported from `error_taxonomy_e2e.rs`) ───────────

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
// Ported verbatim (behaviourally) from `error_taxonomy_e2e.rs`/`two_peer_e2e.rs`:
// reader threads drain stdout/stderr so the child never blocks on a full pipe,
// and `Drop` kills the child so no orphan survives a panic or early return.

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
                     --- stdout ---\n{}",
                    self.stdout.lock().expect("capture buffer not poisoned"),
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// As [`Self::wait_for_line`], but polls the child's **stderr** capture — the
    /// stream `diag:` lines land on (spec §5.3: verbose diagnostics are additive,
    /// stderr-only).
    fn wait_for_stderr_line(&self, needle: &str, timeout: Duration) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = Self::scan(&self.stderr, needle) {
                return Ok(line);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {needle:?} on stderr\n\
                     --- stderr ---\n{}",
                    self.stderr.lock().expect("capture buffer not poisoned"),
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
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

// ── fixture: a converged two-member room with Alice kept alive ────────────────
//
// Unlike `error_taxonomy_e2e.rs::converge_two_member_room` (which tears Alice's
// session down once Bob has joined, because its callers only need Bob's
// already-persisted local state), the diagnostics block needs a *second* live
// dial while Alice is still up: `room members --status --verbose` classifies
// Bob's connection to Alice from `Endpoint::remote_info`, which is only
// populated once a real, live QUIC session exists. That second dial is the same
// "one one-shot process dialing one already-live listener" shape as the initial
// join (proven CI-safe by `error_taxonomy_e2e.rs`), not the riskier
// teardown-then-redial shape that file reserves for its `#[ignore]`-gated tier.
struct LiveConverged {
    // Kept alive so the on-disk home outlives the `alice_tail` process (its data
    // dir must not be deleted out from under a still-running child), and so
    // `alice_tail` itself stays live (Drop kills it) for the duration of every
    // test that borrows `LiveConverged` — neither field is read directly.
    #[allow(dead_code)]
    alice_home: TempDir,
    #[allow(dead_code)]
    alice_tail: ChildSession,
    alice_addr: String,
    bob_home: TempDir,
    room: String,
}

fn converge_with_alice_alive() -> LiveConverged {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    identity_create(&alice_home, "Alice");
    identity_create(&bob_home, "Bob");
    let bob_id = identity_field(&bob_home, "identity_id");
    let room = room_create(&alice_home, "Diagnostics Room");
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

    LiveConverged {
        alice_home,
        alice_tail,
        alice_addr,
        bob_home,
        room,
    }
}

/// Run `room members <ROOM_ID> --status [--verbose] --peer <alice> --loopback`
/// from Bob's home while Alice is still live.
fn members_status(live: &LiveConverged, verbose: bool) -> std::process::Output {
    let mut args = vec![
        "room",
        "members",
        &live.room,
        "--status",
        "--peer",
        &live.alice_addr,
        "--timeout",
        "10s",
        "--loopback",
    ];
    if verbose {
        args.push("--verbose");
    }
    one_shot(live.bob_home.path(), &args)
}

const PATH_LABELS: [&str; 4] = ["path=direct", "path=relay", "path=mixed", "path=none"];

// ══ spec §8 #5: the diag block is present under --verbose, valid path=, hidden by default ══

/// `room members --status --verbose` against a genuinely live peer prints the
/// three-line `diag:` block (local / per-peer / transport summary) on stderr,
/// with a valid `path=` classification; the identical command *without*
/// `--verbose` carries no `diag:` line at all (§18.5 "hide unless asked").
#[test]
fn members_status_verbose_renders_diag_block_default_hides_it() {
    let live = converge_with_alice_alive();

    let verbose_out = members_status(&live, true);
    assert!(
        verbose_out.status.success(),
        "members --status --verbose must exit 0; stderr: {}",
        String::from_utf8_lossy(&verbose_out.stderr)
    );
    let stderr = String::from_utf8_lossy(&verbose_out.stderr);
    assert!(
        stderr.lines().any(|l| l.starts_with("diag: local id=")),
        "must render the `diag: local id=…` line under --verbose; stderr:\n{stderr}"
    );
    let peer_line = stderr
        .lines()
        .find(|l| l.starts_with("diag: peer "))
        .unwrap_or_else(|| {
            panic!("must render a `diag: peer …` line under --verbose; stderr:\n{stderr}")
        });
    assert!(
        PATH_LABELS.iter().any(|label| peer_line.contains(label)),
        "diag: peer line must carry one of {PATH_LABELS:?}; got: {peer_line}"
    );
    assert!(
        stderr
            .lines()
            .any(|l| l.starts_with("diag: transport connected=")),
        "must render the `diag: transport connected=…` summary line under --verbose; stderr:\n{stderr}"
    );

    // Default (no --verbose): the same live session must carry no diag: line at all.
    let default_out = members_status(&live, false);
    assert!(
        default_out.status.success(),
        "members --status (no --verbose) must exit 0; stderr: {}",
        String::from_utf8_lossy(&default_out.stderr)
    );
    let default_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&default_out.stdout),
        String::from_utf8_lossy(&default_out.stderr)
    );
    assert!(
        !default_combined.contains("diag:"),
        "a non-verbose run must carry no diag: line on either stream; got:\n{default_combined}"
    );
}

// ══ spec §8 #6: --verbose is stderr-only (stdout stays script-clean) ══════════

/// The `diag:` block is additive on stderr; stdout under `--verbose` still
/// carries only the ordinary `member:`/`peers:` connection panel, with zero
/// `diag:` lines mixed in (so a script parsing stdout is unaffected).
#[test]
fn members_status_verbose_diag_lines_are_stderr_only() {
    let live = converge_with_alice_alive();

    let out = members_status(&live, true);
    assert!(
        out.status.success(),
        "members --status --verbose must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("diag:"),
        "stdout must stay script-clean under --verbose (diag: is stderr-only); got:\n{stdout}"
    );
    assert!(
        stdout.contains("member:") && stdout.contains("peers:"),
        "stdout must still carry the ordinary connection panel under --verbose; got:\n{stdout}"
    );
}

// ══ spec §8 #7 / AC3: verbose diagnostics leak no identity/device secret seed ═

/// The load-bearing AC3 property for the new diagnostics surface: a live
/// `--verbose` run renders only public identifiers (`EndpointId`, relay urls,
/// state labels) — never the local identity's or device's secret signing seed,
/// on either stream. `corrupted_ticket_never_echoes_token_or_secret` in
/// `error_taxonomy.rs` covers the ticket-secret half of AC3 offline; this
/// covers the identity/device-seed half specifically through the new live
/// `diag:` render path.
#[test]
fn members_status_verbose_leaks_no_identity_or_device_seed() {
    let live = converge_with_alice_alive();

    let secret_raw = std::fs::read_to_string(live.bob_home.path().join("identity.secret"))
        .expect("bob's identity.secret must exist after identity create");
    let secret_v: serde_json::Value =
        serde_json::from_str(&secret_raw).expect("parse bob's identity.secret");
    let identity_seed = secret_v["identity_secret"]
        .as_str()
        .expect("identity_secret field")
        .to_owned();
    let device_seed = secret_v["device_secret"]
        .as_str()
        .expect("device_secret field")
        .to_owned();

    let out = members_status(&live, true);
    assert!(
        out.status.success(),
        "members --status --verbose must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for (label, seed) in [("identity", &identity_seed), ("device", &device_seed)] {
        assert!(
            !stdout.contains(seed.as_str()),
            "stdout must never contain the {label} secret seed under --verbose"
        );
        assert!(
            !stderr.contains(seed.as_str()),
            "stderr (including the diag: block) must never contain the {label} secret seed \
             under --verbose"
        );
    }
}

// ══ spec §5.3 "also" anchor: `room tail --verbose` renders the diag block too ═

/// Spec §5.3 names **two** anchors for `--verbose`: the primary `room members
/// --status --verbose` (covered above) and, "also", `room tail --verbose`. Unlike
/// `members --status`, `tail` is a long-running live-view command whose
/// diagnostics render exactly once at startup (`message.rs::tail`, right after the
/// `room: <id>` line, before the display loop begins) — a different code path from
/// `members_status`'s post-settle render. `error_taxonomy.rs`'s
/// `room_tail_accepts_verbose_and_parses_room_id_first` only proves `--verbose` is
/// *wired* at the clap level, network-free (its own doc comment defers the actual
/// render to "the e2e phase", same as the `members` flag tests deferred to this
/// file). This closes that gap: a real `room tail --verbose` against a live peer
/// renders the `diag:` block on stderr, and a plain `room tail` (no `--verbose`)
/// against the same live peer renders none.
#[test]
fn room_tail_verbose_renders_diag_block_default_hides_it() {
    let live = converge_with_alice_alive();

    let bob_verbose = ChildSession::spawn(
        live.bob_home.path(),
        &[
            "room",
            "tail",
            &live.room,
            "--peer",
            &live.alice_addr,
            "--verbose",
            "--loopback",
        ],
    );
    bob_verbose
        .wait_for_line("room:", WAIT)
        .unwrap_or_else(|err| panic!("bob's verbose tail never printed room: {err}"));
    let local_line = bob_verbose
        .wait_for_stderr_line("diag: local id=", WAIT)
        .unwrap_or_else(|err| {
            panic!("bob's verbose tail never rendered `diag: local id=…`: {err}")
        });
    assert!(
        local_line.starts_with("diag: local id="),
        "got: {local_line}"
    );
    let transport_line = bob_verbose
        .wait_for_stderr_line("diag: transport connected=", WAIT)
        .unwrap_or_else(|err| {
            panic!("bob's verbose tail never rendered the `diag: transport …` summary: {err}")
        });
    assert!(
        transport_line.starts_with("diag: transport connected="),
        "got: {transport_line}"
    );
    // Any `diag: peer …` line present must carry a valid path= classification (the
    // room's only other member, Alice, is already a known peer entry by startup).
    if let Some(peer_line) = ChildSession::scan(&bob_verbose.stderr, "diag: peer ") {
        assert!(
            PATH_LABELS.iter().any(|label| peer_line.contains(label)),
            "diag: peer line must carry one of {PATH_LABELS:?}; got: {peer_line}"
        );
    }
    drop(bob_verbose);

    // Default (no --verbose): the identical live tail carries no diag: line on
    // either stream (§18.5 "hide networking details unless needed").
    let bob_default = ChildSession::spawn(
        live.bob_home.path(),
        &[
            "room",
            "tail",
            &live.room,
            "--peer",
            &live.alice_addr,
            "--loopback",
        ],
    );
    bob_default
        .wait_for_line("room:", WAIT)
        .unwrap_or_else(|err| panic!("bob's default tail never printed room: {err}"));
    // Give the (absent) diagnostics render the same window it used above before
    // asserting its absence, so this isn't just "we didn't wait long enough".
    thread::sleep(Duration::from_millis(500));
    let stdout = bob_default
        .stdout
        .lock()
        .expect("capture buffer not poisoned")
        .clone();
    let stderr = bob_default
        .stderr
        .lock()
        .expect("capture buffer not poisoned")
        .clone();
    assert!(
        !stdout.contains("diag:") && !stderr.contains("diag:"),
        "a non-verbose tail must carry no diag: line on either stream; \
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
