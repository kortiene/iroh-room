# IR-0109 — Two-Peer Integration Test

- **Issue:** #24 `[IR-0109] Add two-peer integration test`
- **Labels:** `type/test` `area/cli` `area/dx` `priority/p0` `risk/medium`
- **Parent:** #2
- **Traceability:** `PRD.v0.3.md` §17.1 (Technical Success Metrics), §17.2 (DX Metrics), §19 Phase 1A deliverable 8 ("Integration test for two peers").
- **Dependencies (all landed):** #16 identity CLI (IR-0101), #17 room create (IR-0102), #18 invite (IR-0103), #19 join (IR-0104), #20 message send/tail (IR-0105), #21 offline room-read (IR-0106), #22 peer connection manager (IR-0107), #23 pipe reconcile (IR-0108).
- **Status:** Implemented. `crates/iroh-rooms-cli/tests/two_peer_e2e.rs` and the `Cargo.toml` dev-dep line have landed (issue #24).

---

## 1. Summary

Phase 1A is functionally complete: every user-facing slice (identity, room, invite/join,
messaging, live pipe) has landed with its own unit and Node-API end-to-end tests. What is
still missing — and what PRD §19 Phase 1A deliverable 8 names explicitly — is a **single
integration test that chains the whole slice together and proves it works as a product**:
two isolated participants, driven through the real `iroh-rooms` binary, converging on the
same room and exchanging a message and a live pipe **without a central application server**.

This spec adds that test as a new CLI integration suite, **`crates/iroh-rooms-cli/tests/two_peer_e2e.rs`**,
that drives the built binary across **two isolated on-disk homes** over the CLI's existing
`--loopback` network stack (`NetMode::Loopback` = `presets::Minimal` + `RelayMode::Disabled`,
no relay, no discovery) with explicit `--peer` addressing. The suite is **tiered by CI
reliability**: the deterministic, network-free assertions (restart persistence, local-first
no-server operation) always run in CI; the live two-process online assertions (membership
convergence, live pipe) run in CI when reliable and are otherwise **`#[ignore]`-gated local
tests with a documented command**, exactly as the issue Test Plan permits. The already-landed
Node-API suites (`join_e2e.rs`, `message_e2e.rs`, `pipe_e2e.rs`) remain the always-green CI
backstop for the same acceptance criteria at the lower layer.

No production code is modified. All wiring the test needs (`--loopback`, `--peer`,
`--accept-joins`, `room tail --offline --json`, `room members --json`, the `pipe expose`
stderr audit sink) already ships.

---

## 2. Context: what already exists

### 2.1 The CLI surface (all shipped, reconciled to the binary in `docs/getting-started.md`)

| Step | Command | Shape relevant to the test |
|---|---|---|
| Identity | `iroh-rooms identity create --name <NAME>` / `identity show [--json]` | writes `<home>/identity.json` + `identity.secret`; `show --json` → `{"name","identity_id","device_id"}` |
| Room create | `iroh-rooms room create <NAME>` | prints `room_id: blake3:<hex>`, `admin: <id>`; persists genesis to `<home>/rooms.db` |
| Members | `iroh-rooms room members <ROOM_ID> [--json]` | **offline** fold of local log; `--json` → `{"room","admin","members":[{identity_id,role,status,is_admin}]}` |
| Invite | `iroh-rooms room invite <ROOM_ID> --invitee <ID> [--role member\|agent] [--expires <DUR>]` | prints `invite_id:`, `role:`, `expires:`, then `ticket:` followed by an indented `  roomtkt1…` token line |
| Join | `iroh-rooms room join <TICKET> [--peer <ADDR>]… [--loopback] [--timeout <DUR>]` | one-shot; dials admin, pulls membership sub-DAG, publishes `member.joined`; prints `joined:`, `members: N active`; **fails** if the admin never observes the join |
| Tail (online host) | `iroh-rooms room tail <ROOM_ID> --accept-joins [--peer <ADDR>]… [--loopback]` | **long-running**; prints `listening: <ENDPOINT_ID>@<ip:port>` then `room: …` then streams until Ctrl-C/SIGTERM; `--accept-joins` opens the provisional bootstrap window (admin only) |
| Send | `iroh-rooms room send <ROOM_ID> <MSG> [--peer <ADDR>]… [--loopback]` | one-shot, offline-first; always `stored: yes`; prints `sent: <event_id>`, `delivered: N …` |
| Tail (offline read) | `iroh-rooms room tail <ROOM_ID> --offline [--json] [--limit N]` | **one-shot**, network-free; renders all validated event types incl. `message.text` bodies in canonical `(lamport,event_id)` order |
| Pipe expose | `iroh-rooms pipe expose <ROOM_ID> --tcp 127.0.0.1:<port> --allow <ID>… [--loopback]` | **long-running**; ⚠ SECURITY lines + rejects go to **stderr**; prints `listening:` and `pipe_id: <32-hex>` to stdout |
| Pipe connect | `iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local <port> [--peer <ADDR>]… [--loopback]` | **long-running**; binds `127.0.0.1:<port>` and forwards to the owner |
| Pipe close | `iroh-rooms pipe close <PIPE_ID> [--room <ROOM_ID>]` | one-shot |

Key enablers for a CI-safe two-process test:

- **`--loopback`** (hidden flag, `#[arg(long, hide = true)]`) on every online command routes
  through `net_mode(loopback) → NetMode::Loopback` (`crates/iroh-rooms-cli/src/message.rs:674`),
  which builds `Endpoint::builder(presets::Minimal)` with `RelayMode::Disabled`
  (`crates/iroh-rooms-net/src/transport.rs:233`). No relay server, no n0 discovery — pure
  loopback QUIC over `127.0.0.1`. This is the same deterministic stack the in-process Node
  e2e tests use, just across two OS processes.
- **`--peer <ENDPOINT_ID>[@<ip:port>]`** supplies an explicit dial address, so no discovery
  is required. The long-running host commands print their dialable address on a `listening:`
  line, which the harness parses and threads into the peer's `--peer`.
- **On-disk isolation**: each participant is a distinct `--data-dir <PATH>` (or
  `IROH_ROOMS_HOME`) → distinct `<home>/rooms.db`, satisfying scope bullet 1 ("two isolated
  local homes/databases") and enabling **restart** by simply spawning a fresh process against
  the same directory.

### 2.2 Landed Node-API backstops (the reliable CI oracle for the same ACs)

- `crates/iroh-rooms-net/tests/join_e2e.rs` — two in-process loopback nodes; a valid
  `member.joined` converges so **both** peers show the joiner `Active` (AC2 at the Node layer),
  plus bad-secret / expired-invite rejection.
- `crates/iroh-rooms-net/tests/message_e2e.rs` — two-peer signed `message.text` round-trip.
- `crates/iroh-rooms-net/tests/pipe_e2e.rs` — P1 authorized member round-trips bytes (AC4),
  P2/P3 unauthorized/non-member denied with zero bytes forwarded (AC5), P10 owner-side audit
  sink records `connect_rejected:not_allowed`.

These are fast, deterministic, `RelayMode::Disabled`, and every await is timeout-bounded.
This spec **reuses their fixture patterns** (`Principal`, `build_room`, `spawn_echo_server`,
`wait_*` helpers) and treats them as the always-green lower-layer coverage; it does **not**
recreate them.

### 2.3 Existing CLI test conventions to mirror

- `assert_cmd::Command::cargo_bin("iroh-rooms")` for one-shot commands;
  `env_remove("IROH_ROOMS_HOME")` then `--data-dir <tempdir>` for isolation
  (`crates/iroh-rooms-cli/tests/message_cli.rs:17`).
- `predicates` for stdout/stderr assertions; `tempfile::TempDir` per home.
- `extract_field(output, "room_id")`-style line parsing for `key: value` output.
- `tests/docs_conformance.rs` shows structural/no-network assertion style; the message/room/
  tail CLI suites show the offline-command style.

### 2.4 Constraints from repository memory

- **verify.sh is the real CI gate**: the new test must pass `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings` (pedantic), and
  `cargo test --workspace --all-targets --all-features`. Any always-CI test must therefore be
  loopback-only, deterministic, and clippy-pedantic clean.
- **CLI has no tracing subscriber**: `TracingAudit` output is dropped on the CLI, so an
  audit-based assertion cannot rely on tracing logs. For AC5 the observable, CLI-native signal
  is the **`pipe expose` stderr audit sink** installed by `Node::spawn_with_pipe_audit`
  (IR-0108): an unauthorized connect is rejected **and** printed to the owner's stderr as
  `pipe.connect.rejected:<cause>`.
- **Membership snapshot ignores content events** / **member-message ancestor-view gate**:
  membership convergence must be asserted over `Member*` events (via `room members`), not
  inferred from message events; a message from a non-member is silently rejected. The test
  asserts convergence through `room members --json`, which is the fold-derived roster.

---

## 3. Goals / Non-goals

### Goals

1. One cohesive, product-level integration test proving the full Phase 1A slice across two
   isolated participants driven through the **real `iroh-rooms` binary**.
2. Cover all five issue acceptance criteria, each mapped to a named test function.
3. Run the reliable subset in CI unconditionally; gate the flaky-prone live subset behind
   `#[ignore]` with a documented command — never let this suite make `verify.sh` flaky.
4. No external tool dependencies (no `python3`, no `curl`): the pipe target service and the
   traffic client are in-test loopback TCP, mirroring `pipe_e2e.rs`.
5. Self-documenting: the module docs carry an AC→test map and the exact gated-run command.

### Non-goals

- **Real-NAT / two-machine connectivity.** That is Gate A (`spike-nat`, IR-0012) and is out of
  scope; the canonical test runs single-host over loopback (PRD demo path). A two-machine
  variant is documented only as optional prose.
- **Re-testing lower-layer correctness** (signature validation, fold determinism, sync
  windowing). Those have dedicated conformance suites; this test asserts the *integration*.
- **The `file` and `agent` planes.** They are scaffold in Phase 1A (not recognized by the
  binary) and are Phase 1B scope; the two-peer test covers only shipped commands.
- Modifying any production code or CLI surface. Everything needed already ships.

---

## 4. Owning module & new files

| Path | Kind | Purpose |
|---|---|---|
| `crates/iroh-rooms-cli/tests/two_peer_e2e.rs` | **new** integration test | The primary deliverable: the product-slice two-peer test + child-process harness. |
| `crates/iroh-rooms-cli/Cargo.toml` | edit (`[dev-dependencies]` only) | Add `tokio` (rt + net + macros + time) as a dev-dep for the in-test loopback target server / TCP client used by the pipe tier. `assert_cmd`, `predicates`, `tempfile`, `serde_json` already present. |
| `docs/getting-started.md` (or a short note) | optional doc edit | One line under the Status section pointing at the gated-run command. Non-blocking. |

No new production modules. No new crate. The Node-API backstops already live in
`crates/iroh-rooms-net/tests/`.

> **Dev-dependency note.** The test spawns the binary as a child process (`std::process::Command`)
> and, for the pipe tier, runs a loopback TCP echo/HTTP target and a TCP client inside the test
> process. Those in-test sockets want an async runtime; add `tokio` to the CLI crate's
> `[dev-dependencies]` (it is already a normal dependency of the crate, so no new lockfile
> churn). Child-process orchestration itself uses `std::process` + threads and needs no runtime.

---

## 5. Design decisions

**D1 — Primary layer is the CLI process, not the Node API.** The issue says "prove Phase 1A
works **as a product slice**" and scope bullet 1 is "two isolated local homes/databases." Both
point at the shipped binary over on-disk stores, not in-process `Node`s. The Node layer is
already covered by `join_e2e`/`message_e2e`/`pipe_e2e`; the unique value of #24 is the
end-to-end binary-level chain. → New suite lives in `crates/iroh-rooms-cli/tests/`.

**D2 — Loopback + explicit `--peer`, never real network.** All online commands run with the
hidden `--loopback` flag (`RelayMode::Disabled`, `presets::Minimal`) and are wired together by
parsing each host's `listening:` address and passing it as the peer's `--peer`. This makes the
online tier hermetic (no relay, no DNS, no discovery), which is what makes it *eligible* to run
in CI at all and is also the literal proof of AC1 ("without central application server").

**D3 — Tier by reliability, gate the rest.** The issue Test Plan authorizes exactly this:
"Automated integration test in CI if reliable; otherwise gated local test with documented
command." Two tiers:

- **CI tier (always run, `#[test]` / `#[tokio::test]`):** deterministic, no live cross-process
  networking — restart persistence (AC3) and local-first no-server operation (AC1). These are
  offline reads/writes over `rooms.db`; they cannot flake.
- **Online tier (`#[ignore]`, documented command):** anything requiring two live processes to
  rendezvous over loopback QUIC — membership convergence (AC2) and the live pipe (AC4/AC5).
  Marked `#[ignore]` so `verify.sh` stays green regardless of the CI runner's networking; run
  locally (and in a dedicated, non-blocking CI job if desired) with the documented command.

  The same ACs are covered green-in-CI at the Node layer by the existing backstops, so gating
  the CLI online tier loses **no** guaranteed coverage — it adds product-level coverage on top.

  > **Escalation note.** If the online tier proves reliable on the project's CI runner (loopback
  > QUIC across two child processes is the same transport the in-process e2e tests already run
  > green), a follow-up may drop `#[ignore]` from the convergence test. Start conservative:
  > ship it gated, measure, then promote. Document the current tier of each test in the module
  > docs so the state is never ambiguous.

**D4 — No external tools; in-test loopback target + client.** The pipe tier must not depend on
`python3 -m http.server` or `curl` (portability + CI). Instead the test spawns an in-process
tokio TCP **echo** (or minimal HTTP) server bound to `127.0.0.1:0` as the `--tcp` target and
uses an in-process `TcpStream` to the connector's `--local` port to drive the round-trip —
exactly the `spawn_echo_server` / `TcpStream` pattern from `pipe_e2e.rs`. Zero external deps.

**D5 — Restart = a fresh process against the same `--data-dir`.** AC3 ("message persists across
restart") is proven the truest and most deterministic way: write with one `iroh-rooms` invocation,
then read it back with a **separate** `iroh-rooms room tail --offline --json` invocation over the
same home. A new OS process with a cold store is a real restart; the offline read is network-free
and byte-stable. This needs no live peer, so it lives in the CI tier.

**D6 — Convergence asserted via `room members --json` on both homes.** AC2 ("both peers agree on
room membership") is a fold-derived property; the canonical oracle is each home's roster. After
the join completes, the test parses `room members --json` from **both** Alice's and Bob's homes
and asserts the two rosters are set-equal (same admin, same `{identity_id, role, status}`
membership, both `active`). Reading `--json` avoids brittle text parsing (memory: membership is
derived from `Member*` events, not content events — the roster is the authority).

**D7 — `ChildSession` harness with kill-on-drop and bounded readiness wait.** Long-running
commands (`room tail --accept-joins`, `pipe expose`, `pipe connect`) are not one-shot, so
`assert_cmd`'s `.assert()` model does not fit. A small `ChildSession` helper wraps
`std::process::Command` with `Stdio::piped()`, streams the child's stdout on a reader thread
into a shared buffer, exposes `wait_for_line(substr, timeout)` to block until (e.g.) the
`listening:` / `pipe_id:` line appears, exposes captured stdout/stderr, and implements `Drop`
to send SIGTERM (then SIGKILL after a grace period) so no orphan process survives a panic or an
early return. Every wait is timeout-bounded so a rendezvous bug fails fast instead of hanging CI.

**D8 — AC5 is a 2-member room; the signal is the two CLI-native denial lines.** No third
participant is needed. `pipe connect`'s only pre-check is that the *connector* is an Active
member (`pipe.rs:234`); the **allow-list is enforced owner-side** by `pipe_connect_allowed`
(the landed two-stage gate). So the cleanest unauthorized case is: in the converged
`{Alice, Bob}` room, Alice exposes allowing **an id other than Bob**, and Bob — Active but not
allow-listed — connects. Because the CLI has no tracing subscriber (memory), the observable,
CLI-native proof that the connect was *denied and logged* is a pair of stderr lines: the owner's
`pipe expose` prints `pipe.connect.rejected:not_allowed` from the IR-0108 audit sink, and the
connector's `pipe connect` prints `[pipe] denied by the owner (not authorized / closed)`
(`pipe.rs:308`). The test asserts (a) the connector's traffic never round-trips (no echo bytes)
and (b) both denial lines appear. This mirrors the Node-layer backstop
`pipe_e2e::p2_non_allowlisted_member_is_denied` ("Carol, Active, not allowed → denied
`not_allowed`, zero bytes forwarded").

**D9 — Serialize the online tier (`--test-threads` guidance) and bound every wait.** Two live
loopback sessions per test plus in-test servers are resource-sensitive; the documented run
command pins `--test-threads=1` for the online tier and every network wait is `Duration`-bounded
(15 s budget, matching the e2e suites) so failures surface as assertion errors, not hangs.

**D10 — Ticket + id parsing helpers.** `room invite` prints the ticket on an indented line under
`ticket:`; the invite requires Bob's `identity_id` from `identity show --json`. Add small
parsers: `identity_id_of(home)` (parse `identity show --json`), `extract_ticket(stdout)` (first
line token starting `roomtkt1`), and reuse the `extract_field` helper for `room_id`/`pipe_id`.

---

## 6. Test architecture

### 6.1 The `ChildSession` harness (core engineering component)

A test-only helper (top of `two_peer_e2e.rs`), roughly:

```rust
/// A spawned long-running `iroh-rooms` session (room tail / pipe expose / pipe connect).
/// Streams stdout+stderr into shared buffers on reader threads; kills the child on drop.
struct ChildSession {
    child: std::process::Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl ChildSession {
    /// Spawn `iroh-rooms <args…>` with the given data dir; piped stdout/stderr.
    fn spawn(data_dir: &Path, args: &[&str]) -> ChildSession { /* Command + threads */ }

    /// Block until a captured stdout line contains `needle`, or `timeout` elapses.
    /// Returns the full matching line (so the caller can parse `listening:` / `pipe_id:`).
    fn wait_for_line(&self, needle: &str, timeout: Duration) -> anyhow::Result<String> { /* poll */ }

    fn stdout_snapshot(&self) -> String { /* lock+clone */ }
    fn stderr_snapshot(&self) -> String { /* lock+clone */ }
}

impl Drop for ChildSession {
    fn drop(&mut self) {
        // SIGTERM (graceful: publishes pipe.closed{owner_exit}), then SIGKILL after a grace.
    }
}
```

Notes:
- Use the built binary path via `assert_cmd::cargo::cargo_bin("iroh-rooms")` (works for
  `std::process::Command` too) so the test targets the same artifact.
- Graceful SIGTERM first (so `pipe expose` emits `pipe.closed{owner_exit}` per IR-0108),
  escalate to SIGKILL if the child doesn't exit within a short grace window. On Unix use
  `libc`/`nix`-free `Child::kill()` for SIGKILL; for SIGTERM, either send via a tiny
  `std::os::unix` `kill(2)` shim or accept `Child::kill()` (SIGKILL) as the simpler,
  portable default and document that the CLI's SIGKILL path still stops forwarding (the pipe
  then lingers on the log until an owner/admin close — acceptable inside a torn-down temp home).
  **Recommendation:** prefer SIGTERM where cheap; SIGKILL is an acceptable fallback because the
  temp homes are discarded at test end.
- Reader threads must drain the pipes to avoid the child blocking on a full stdout buffer
  (important for the streaming `room tail` / `pipe expose`).

### 6.2 Shared fixture helpers

```rust
fn bin() -> Command;                                   // assert_cmd one-shot, IROH_ROOMS_HOME removed
fn one_shot(dir, &[args]) -> Output;                   // run + capture, assert success where expected
fn identity_create(dir, name);                         // identity create --name
fn identity_id(dir) -> String;                         // parse `identity show --json`.identity_id
fn room_create(dir, name) -> String;                   // → room_id
fn invite(dir, room, invitee_id, role, expires) -> String; // → roomtkt1… ticket
fn members_json(dir, room) -> RosterJson;              // room members --json → parsed
fn extract_field(out, key) -> Option<&str>;            // reuse message_cli helper
fn extract_ticket(out) -> String;                      // first token starting "roomtkt1"
fn parse_listening(line) -> String;                    // "listening: <ENDPOINT_ID>@<ip:port>" → addr
```

`RosterJson` = a `serde_json`-parsed `{room, admin, members:[{identity_id, role, status, is_admin}]}`
compared as a set (order-independent).

### 6.3 Per-AC test functions

| Issue AC | Test fn | Tier | Mechanism |
|---|---|---|---|
| **AC1** — completes without central server | `full_slice_runs_without_central_server` | **CI** | Runs the offline backbone (identity → room → invite → offline send → offline read) using only local `--data-dir` stores; the harness starts **no** server and passes `--loopback` (RelayMode::Disabled) on any online step. Asserts the flow succeeds with no relay/discovery reachable. Documented as the local-first invariant. |
| **AC2** — both peers agree on membership | `two_peers_converge_on_membership` | **`#[ignore]`** (online) | Alice hosts `room tail --accept-joins --loopback` (`ChildSession`); parse her `listening:` addr; Bob `room join <ticket> --peer <alice> --loopback` (one-shot, must exit 0). Stop Alice's session. Assert `members_json(alice)` and `members_json(bob)` are set-equal: admin=Alice, members = {Alice admin/active, Bob member/active}. |
| **AC3** — message persists across restart | `message_persists_across_restart` | **CI** | In one home create identity+room, `room send <room> "<msg>"` (offline → `stored: yes`). Then a **fresh process** `room tail <room> --offline --json` reads the log and the JSON array contains a `message.text` row with `body == "<msg>"`. No network. |
| **AC4** — pipe works for authorized peer | `authorized_pipe_forwards_bytes` | **`#[ignore]`** (online) | Build a converged 2-member room (reuse AC2 setup); spawn in-test loopback echo server; Alice `pipe expose <room> --tcp 127.0.0.1:<echo> --allow <bob_id> --loopback` (`ChildSession`, parse `pipe_id:` + `listening:`); Bob `pipe connect <room> <pipe_id> --local 0 --peer <alice> --loopback` (`ChildSession`); parse Bob's bound `127.0.0.1:<p>` from his `forwarding:` line; open `TcpStream` to `127.0.0.1:<p>`, write `ping`, read `ping` back → assert round-trip. |
| **AC5** — unauthorized connection denied | `unauthorized_pipe_connection_denied` | **`#[ignore]`** (online) | **Same 2-member room** — no third participant needed. Alice exposes allowing an id **other than Bob** (e.g. Alice's own `identity_id`, or a fabricated member id); Bob is an **Active member but not in `--allow`**, so his connect passes the CLI's active-member pre-check yet is denied **owner-side** by `pipe_connect_allowed`. Assert (a) no echo bytes round-trip through Bob's `--local` port and (b) two CLI-native denial signals: Alice's `ChildSession.stderr_snapshot()` contains `pipe.connect.rejected:not_allowed` (IR-0108 audit sink) **and** Bob's `stderr_snapshot()` contains `[pipe] denied by the owner`. |

`full_slice_end_to_end` (optional, `#[ignore]`): a single narrative test that runs the entire
chain AC1→AC5 in order against one pair of homes, as the human-readable "product slice" demo.
The per-AC tests above are the granular oracles; this one is the story. Keep it `#[ignore]`.

### 6.4 The "no central server" assertion, concretely

AC1 is partly a structural/observable property. The test makes it observable by:
- Never spawning any relay/server/broker process in the harness (only the two `iroh-rooms`
  children + an in-test loopback target for the pipe tier, which is the *service being
  exposed*, not application infrastructure).
- Passing `--loopback` on every online command → `RelayMode::Disabled` (no relay is even
  reachable), asserted indirectly by the online tier succeeding with only `--peer` direct dials.
- Proving the offline backbone (create/invite/send/read) works with the machine's network
  effectively unused — local-first.
- A module doc comment stating the invariant and pointing at
  `transport.rs:233` (`RelayMode::Disabled`) as the code-level guarantee.

---

## 7. Implementation steps

1. **Add the dev-dependency.** In `crates/iroh-rooms-cli/Cargo.toml` `[dev-dependencies]`, add
   `tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "net", "io-util"] }`
   (the crate already depends on tokio as a normal dep, so no new resolution). `libc` is **not**
   required if SIGKILL-on-drop is accepted; add it only if SIGTERM-on-drop is implemented.

2. **Create `crates/iroh-rooms-cli/tests/two_peer_e2e.rs`** with a module doc comment that
   contains: the AC→test map (§6.3), the tier of each test, the exact gated-run command (§8),
   and the AC1 no-server invariant note.

3. **Implement the fixture helpers (§6.2)** — `bin`, `one_shot`, `identity_create`,
   `identity_id`, `room_create`, `invite`, `members_json`, `extract_field`, `extract_ticket`,
   `parse_listening`. Port `extract_field` verbatim from `message_cli.rs`.

4. **Implement `ChildSession` (§6.1)** — spawn with piped stdio + reader threads draining into
   `Arc<Mutex<String>>`; `wait_for_line` polling with a `Duration` budget; `Drop` that kills the
   child (SIGTERM→SIGKILL if implementing graceful; SIGKILL-only acceptable). Unit-guard: a tiny
   self-test that spawns `iroh-rooms --help` (or `identity create`) and confirms capture works,
   so a harness regression is caught independently of the networked tests.

5. **Write `message_persists_across_restart` (AC3, CI tier).**
   - `identity_create(home, "Alice")`; `room = room_create(home, "Persist Room")`.
   - `one_shot(home, ["room","send",&room,"persisted message"])` → assert success, `stored: yes`.
   - **Fresh process:** `one_shot(home, ["room","tail",&room,"--offline","--json"])` → parse the
     JSON array; assert some row has `type == "message.text"` and `body == "persisted message"`.
   - Assert byte-stability by running the offline read twice and comparing stdout.

6. **Write `full_slice_runs_without_central_server` (AC1, CI tier).**
   - Two homes A and B; identities; `room` in A; `invite(A, room, id(B), "member", None)` →
     ticket (captured, proving the out-of-band capability path).
   - `room send` offline in A; offline `room members --json` and `room tail --offline --json`
     in A both succeed. Assert the whole sequence exits 0 without the harness starting any
     server. (This tier deliberately does **not** perform the live join, so it is deterministic;
     the live convergence is AC2's `#[ignore]` test.)

7. **Write `two_peers_converge_on_membership` (AC2, `#[ignore]`).**
   - Homes A (Alice) and B (Bob); identities in both; `room = room_create(A)`;
     `bob_id = identity_id(B)`; `ticket = invite(A, room, bob_id, "member", Some("24h"))`.
   - `let alice_tail = ChildSession::spawn(A, ["room","tail",&room,"--accept-joins","--loopback"]);`
     `let line = alice_tail.wait_for_line("listening:", WAIT)?;`
     `let alice_addr = parse_listening(&line);`
   - `one_shot(B, ["room","join",&ticket,"--peer",&alice_addr,"--loopback"])` → assert exit 0
     and stdout `members: 2 active`.
   - Poll `members_json(A, room)` until it shows Bob active (bounded) — the join returns only
     after the admin observed it, but the tail child persists asynchronously; poll to be safe.
   - `drop(alice_tail);` (graceful stop). Assert `members_json(A)` == `members_json(B)` as sets:
     admin = Alice, members = {(Alice, admin, active), (Bob, member, active)}.

8. **Write `authorized_pipe_forwards_bytes` (AC4, `#[ignore]`, `#[tokio::test]`).**
   - Reuse a `converge(room, A, B)` helper (extracted from step 7) so both homes are 2-member
     active. (The pipe requires Bob to be an Active member.)
   - Spawn an in-test tokio echo server on `127.0.0.1:0` → `echo_addr` (mirror
     `pipe_e2e::spawn_echo_server`).
   - `let expose = ChildSession::spawn(A, ["pipe","expose",&room,"--tcp",&echo_addr,"--allow",&bob_id,"--loopback"]);`
     parse `pipe_id:` (via `extract_field` over `expose.stdout_snapshot()` after
     `wait_for_line("pipe_id:", WAIT)`) and the expose `listening:` addr.
   - `let connect = ChildSession::spawn(B, ["pipe","connect",&room,&pipe_id,"--local","0","--peer",&alice_addr,"--loopback"]);`
     — `pipe connect` prints its bound loopback socket on both
     `forwarding: 127.0.0.1:<port> -> pipe <id>` and `connect your client to 127.0.0.1:<port>`
     (`pipe.rs:293-300`, `forwarder.local_addr()`), so `--local 0` (OS-assigned) is fully
     supported; `wait_for_line("forwarding:", WAIT)` then parse the `127.0.0.1:<port>` token.
   - `TcpStream::connect(("127.0.0.1", local_port))`, write `b"ping"`, `read_exact` 4 bytes,
     assert `== b"ping"` within `WAIT`. Drop both sessions.

9. **Write `unauthorized_pipe_connection_denied` (AC5, `#[ignore]`, `#[tokio::test]`).**
   - **Reuse the converged 2-member `{Alice, Bob}` room from step 7 — no third participant.**
     The allow-list is enforced owner-side, and `pipe connect` only pre-checks that the connector
     is Active (`pipe.rs:234`), so a member who is Active-but-not-allowed is the canonical
     unauthorized case (D8).
   - Spawn echo server. `pipe expose <room> --tcp 127.0.0.1:<echo> --allow <NOT_BOB_ID> --loopback`
     where `<NOT_BOB_ID>` is Alice's own `identity_id` (Active) or any valid id ≠ Bob.
   - Bob runs `pipe connect <room> <pipe_id> --local 0 --peer <alice> --loopback`; parse his
     `forwarding:` local port.
   - Assert: a TCP client to Bob's local port gets **no** `ping` echo (EOF/reset within `WAIT`);
     `expose.stderr_snapshot()` contains `pipe.connect.rejected:not_allowed`; and
     `connect.stderr_snapshot()` contains `[pipe] denied by the owner`. Drop sessions.

   > The reliable AC5 coverage also exists green-in-CI at the Node layer
   > (`pipe_e2e::p2_non_allowlisted_member_is_denied`, and the audit-sink rejection recording).
   > The CLI-level AC5 test is the product-slice proof on top; keep it `#[ignore]`.

10. **Optional `full_slice_end_to_end` (`#[ignore]`)** — the single narrative chaining
    AC1→AC5 for a demo-shaped read.

11. **Run the gates.**
    - CI tier: `cargo test -p iroh-rooms-cli --test two_peer_e2e` (runs only non-ignored).
    - Full suite locally: `cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1`.
    - `cargo clippy -p iroh-rooms-cli --all-targets --all-features -- -D warnings` (pedantic-clean).
    - `cargo fmt --all --check`. Then `scripts/verify.sh` end-to-end.

12. **Document the gated command.** Add the exact `-- --ignored` invocation to the test module
    docs and (optionally) a one-line pointer in `docs/getting-started.md` Status section and/or
    `README.md` "Verify" section so a developer can find and run the full two-peer proof.

---

## 8. CI integration & reliability strategy

- **`scripts/verify.sh` / `.github/workflows/verify.yml` are unchanged.** `cargo test --workspace
  --all-targets --all-features` runs the **CI tier** (non-ignored) of `two_peer_e2e.rs`
  automatically. The `#[ignore]` online tier is skipped by default, so CI stays deterministic.
- **Documented gated command** (from the issue Test Plan "gated local test with documented
  command"):

  ```bash
  # Full two-peer product-slice proof (membership convergence + live pipe).
  # Loopback only; no relay, no external tools. Serialize to avoid port/resource contention.
  cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1
  ```

- **Optional non-blocking CI job (recommended follow-up, not required by this issue):** a
  separate workflow job that runs the `--ignored` tier on `ubuntu-latest` with
  `continue-on-error: true` (or as a nightly), surfacing flakes without gating merges. If it
  proves stable over time, promote the convergence test out of `#[ignore]` (D3 escalation note).
- **Every wait is bounded** (`WAIT = Duration::from_secs(15)`, matching the e2e suites) so a
  wiring failure is a fast assertion error, never a CI hang.
- **Backstop invariance:** the Node-layer `join_e2e`/`message_e2e`/`pipe_e2e` suites already run
  green in the CI tier and cover AC2/AC4/AC5 at the transport layer, so gating the CLI online
  tier removes no guaranteed CI coverage.

---

## 9. Acceptance criteria mapping

| Issue AC | Proven by (CLI, this suite) | Tier | Node-layer backstop (already green in CI) |
|---|---|---|---|
| Test completes without central application server | `full_slice_runs_without_central_server` + `--loopback` (`RelayMode::Disabled`) on all online steps | CI | All e2e suites run `NetMode::Loopback` / relay disabled |
| Both peers agree on room membership | `two_peers_converge_on_membership` (both homes' `room members --json` set-equal) | `#[ignore]` | `join_e2e::valid_join_both_peers_show_joiner_active` |
| Message persists across restart | `message_persists_across_restart` (send, then fresh-process `room tail --offline --json`) | CI | `store` restart-determinism tests (`rebuild()`); message_e2e round-trip |
| Pipe connection works for authorized peer | `authorized_pipe_forwards_bytes` (TCP round-trip through the pipe) | `#[ignore]` | `pipe_e2e::p1_authorized_member_forwards_to_local_service` |
| Unauthorized connection is denied | `unauthorized_pipe_connection_denied` (no bytes + owner stderr `pipe.connect.rejected`) | `#[ignore]` | `pipe_e2e::p2_non_allowlisted_member_is_denied`, `p10_audit_sink_records_connect_rejected` |

Every AC has **both** a product-level (CLI) assertion in this suite and a green-in-CI lower-layer
backstop, so no criterion depends solely on a gated test.

---

## 10. Risks & mitigations

| # | Risk | Likelihood | Mitigation |
|---|---|---|---|
| R1 | Two-process loopback QUIC rendezvous is flaky on some CI runners (timing, ephemeral ports). | Medium | Online tier is `#[ignore]` by default (D3); Node-layer backstop stays green; bounded waits fail fast; documented `--test-threads=1`. |
| R2 | Long-running child processes orphan on panic/early-return, leaking ports. | Medium | `ChildSession::Drop` kills the child (SIGTERM→SIGKILL); reader threads drain pipes so children don't block; temp homes discarded. |
| R3 | Parsing streamed stdout for `listening:` / `pipe_id:` races the child's output. | Medium | `wait_for_line` polls a continuously-drained shared buffer with a timeout; parse only after the readiness line is seen. |
| R4 | `pipe connect --local 0` OS-assigned port not surfaced, so the client can't target it. | **Resolved** | `pipe connect` prints the bound socket on `forwarding: 127.0.0.1:<port> …` and `connect your client to 127.0.0.1:<port>` (`pipe.rs:293-300`); parse either line. No fixed-port fallback needed. |
| R5 | AC5 needs a heavy 3-member CLI room. | **Resolved** | AC5 uses the same 2-member room: the connector is Active-but-not-allow-listed (D8), which is the exact `pipe_e2e::p2` denial case. No third join. |
| R6 | SIGTERM graceful stop not portable (Windows). | Low | Dev target is macOS/Linux (getting-started prereqs). Use `Child::kill()` (SIGKILL) as the portable fallback; document the temp-home cleanup makes the lingering-pipe-on-log bound irrelevant. |
| R7 | Clippy pedantic failures in test code (e.g. `unwrap` lint, `must_use`). | Low | Mirror the existing e2e/CLI test style (they pass pedantic); run `clippy --all-targets --all-features -D warnings` before finishing (memory: verify.sh is the real gate). |
| R8 | Binary path resolution differs under `std::process::Command` vs `assert_cmd`. | Low | Resolve once via `assert_cmd::cargo::cargo_bin("iroh-rooms")` and pass that absolute path to `std::process::Command`. |
| R9 | Adding `--ignored` to CI later re-introduces flakiness into the merge gate. | Low | Any promotion is a deliberate, measured follow-up (D3); default stays gated. |

---

## 11. Security, privacy, observability, performance

- **Security invariants under test:** authorized-only pipe forwarding (AC4) and unauthorized
  denial with owner-visible logging (AC5) are the product's most safety-critical behaviors; the
  suite exercises them at the binary boundary. It also implicitly confirms key-bound invites
  (Bob can only join with his ticket) and loopback-only pipe binds (`--tcp 127.0.0.1:…`).
- **Secret hygiene:** the suite must not print or assert on secret material. The ticket carries a
  capability secret; the test passes it opaquely and never logs `identity.secret`. (Existing CLI
  tests already assert secret seeds are absent from output; this suite should not regress that,
  and may add a spot check that stdout of the online flow contains no `identity.secret` bytes.)
- **Observability:** AC5 relies on the IR-0108 stderr audit sink (`pipe.connect.rejected:<cause>`)
  as the CLI-native denial signal, since the CLI installs no tracing subscriber (memory).
- **Privacy:** loopback-only, `RelayMode::Disabled` — no traffic leaves the host; the provisional
  join-bootstrap window (IR-0104) discloses only the secret-free membership sub-DAG, which the
  test does not probe beyond the intended join.
- **Performance:** whole `--ignored` suite target < ~60 s wall clock (each online test is a few
  seconds of rendezvous under a 15 s ceiling). The CI tier is sub-second (offline reads/writes).

---

## 12. Rollout / rollback

- **Rollout:** purely additive — one new test file + a dev-dependency line. No production code,
  no CLI surface, no schema, no migration. Landing it cannot change runtime behavior.
- **Rollback:** delete `two_peer_e2e.rs` and revert the `Cargo.toml` dev-dep line. Zero blast
  radius on shipped crates.
- **Docs reconciliation:** if the binary's exact `pipe connect` local-port output differs from
  assumptions (R4), fix the parser in the test — not the binary — and note it; the binary is the
  source of truth (per `docs/getting-started.md`).

---

## 13. Open questions

- **OQ-1 — Promote AC2 out of `#[ignore]`?** If loopback QUIC across two child processes proves
  reliable on this repo's CI runner (it is the same transport the in-process e2e tests run
  green), the convergence test could run in the CI tier. **Recommendation:** ship gated, add the
  optional non-blocking CI job, measure, then decide. (D3 escalation note.)
- **OQ-2 — In-test target: echo vs minimal HTTP?** A TCP echo is the simplest AC4 oracle; a
  minimal HTTP/1.1 responder (as in `pipe_e2e::spawn_http_server`) is closer to the demo's
  `python3 -m http.server`. **Recommendation:** echo for the round-trip assertion (byte-exact,
  trivial), optionally an HTTP variant for a demo-shaped narrative test.
- **OQ-3 — Should `full_slice_end_to_end` be the primary or a supplement?** A single narrative
  test reads as "the product slice" but couples all ACs into one failure. **Recommendation:**
  keep granular per-AC tests as the oracle; provide the narrative as an optional `#[ignore]`
  supplement.
- **OQ-4 — SIGTERM vs SIGKILL on drop.** SIGTERM lets `pipe expose` emit `pipe.closed{owner_exit}`;
  SIGKILL is simpler and portable. **Recommendation:** SIGKILL-only is acceptable for temp homes;
  implement SIGTERM only if a later test asserts the `owner_exit` close event.
- **OQ-5 — Non-blocking `--ignored` CI job now or later?** Out of scope for #24's required
  deliverable but low-effort. **Recommendation:** follow-up issue.

---

## 14. Assumptions

- **A1** — The hidden `--loopback` flag and `--peer` addressing on the shipped binary behave as
  documented (verified in source: `cli.rs`, `message.rs::net_mode`, `transport.rs`), enabling a
  hermetic two-process test. Any drift is fixed in the test, not the binary.
- **A2** — `room join` exits non-zero if the admin never observes the join (verified in
  `join.rs`), so a successful join implies the admin persisted it; the test still polls to
  absorb the tail child's async persistence window.
- **A3** — `room members --json` and `room tail --offline --json` emit stable, parseable JSON
  (IR-0106), suitable for set-equality and body assertions.
- **A4** — The dev target is macOS/Linux (getting-started prerequisites); Windows child-signal
  portability is out of scope.
- **A5** — Verified in source: `pipe expose` prints `pipe_id:` (32-hex) + a `listening:` address
  to stdout and `pipe.connect.rejected:<cause>` to stderr (IR-0108 audit sink); `pipe connect`
  binds `127.0.0.1:<port>` (OS-assigned for `--local 0`) and prints it on a `forwarding:` line
  (`pipe.rs:293-300`), and prints `[pipe] denied by the owner …` to stderr on an owner denial
  (`pipe.rs:308`). The AC5 denial is enforced owner-side, so an Active-but-not-allow-listed
  connector is the canonical unauthorized case (no third member needed).
- **A6** — Adding `tokio` to `[dev-dependencies]` introduces no new dependency resolution (it is
  already a normal dependency of `iroh-rooms-cli`).
- **A7** — The existing Node-layer e2e suites remain green in CI and are an acceptable backstop
  for the ACs whose CLI-level tests are gated.

---

## 15. Summary of deliverables

1. **`crates/iroh-rooms-cli/tests/two_peer_e2e.rs`** — the product-slice two-peer integration
   test: a `ChildSession` harness, shared fixtures, and five AC-mapped test functions (plus an
   optional narrative test), tiered into an always-CI subset (AC1 no-server, AC3 restart
   persistence) and an `#[ignore]` documented-command subset (AC2 convergence, AC4/AC5 live pipe).
2. **`crates/iroh-rooms-cli/Cargo.toml`** — one `[dev-dependencies]` line (`tokio`).
3. **Documented gated command** in the test module docs (and optionally README/getting-started):
   `cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1`.
4. **No production code changes.** CI (`verify.sh`) runs the deterministic tier automatically;
   the live tier is gated and backed by the existing green Node-layer e2e suites.
```
