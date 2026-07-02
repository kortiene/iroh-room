# IR-0209 — Full Demo Integration Test: Two Humans + One Agent

- **Issue:** #34 `[IR-0209] Add full demo integration test: two humans plus one agent`
- **Labels:** `type/test` `area/blob` `area/pipe` `area/agent` `area/dx` `priority/p0` `risk/high`
- **Parent:** #3
- **Traceability:** `PRD.v0.3.md` §6 (MVP Product Goal — the 10-step demo), §17.1 (Technical Success Metrics), §17.2 (DX Metrics), §19 Phase 1B deliverable 8 ("Integration test for human + agent workflow").
- **Dependencies (all landed):** #24 two-peer integration suite (IR-0109), #26 hardened recent-history sync (IR-0201), #29 file fetch + verification (IR-0204), #32 agent invite flow proof (IR-0207), #33 agent status (IR-0208). Also transitively relies on the whole landed CLI/net stack (identity, room, invite/join, send/tail, pipe, blob, agent status) and the CLI error taxonomy (IR-0110).
- **Status:** Planning. This spec describes a **test-only** addition. No production code is to be written.

---

## 1. Summary

Phase 1A shipped the two-human product slice and its integration test (`two_peer_e2e.rs`,
#24). Phase 1B then completed the MVP surface — recent-history sync (#26), the full Blob
Plane (#27/#28/#29/#30), agent identity and agent status (#31/#32/#33), pipe security
warnings (#23) — each landing with its own unit and end-to-end tests. What Phase 1B still
owes, and what PRD §19 Phase 1B deliverable 8 names explicitly, is **one integration test
that chains the entire MVP demo together with the full cast: two humans and one agent** — the
product-level proof that PRD §6's ten-step demo runs as a single flow, across three isolated
participants, driven through the real `iroh-rooms` binary, **without a central application
server**.

This is *not* a re-run of the two existing online suites. Neither exercises the full cast:
`two_peer_e2e.rs` has no agent, no file/agent legs stitched into one flow; `agent_e2e.rs` has
no second human, no file share, no live pipe. #34 is the **three-participant unified demo** —
`{Alice (admin, human), Bob (human), Agent}` in one room — plus two assertions the prior
suites do not make at full strength:

1. **"All events validate after restart"** across the *entire* event-type diversity the demo
   produces (`room.created`, `member.invited`×2, `member.joined`×2, `message.text`,
   `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status`) — not just a single
   `message.text` (as `two_peer_e2e.rs::message_persists_across_restart` does).
2. **"Agent can post status but has no implicit extra privilege"** — the agent posts a signed
   status *and* is refused an admin-only action from its own home, proving membership grants it
   exactly "active member," nothing more.

This spec adds that test as a new CLI integration suite,
**`crates/iroh-rooms-cli/tests/full_demo_e2e.rs`**, that drives the built binary across **three
isolated on-disk homes** over the CLI's existing hidden `--loopback` network stack
(`NetMode::Loopback` = `presets::Minimal` + `RelayMode::Disabled`, no relay, no discovery) with
explicit `--peer` addressing. It is **tiered by CI reliability** exactly like its siblings: a
deterministic, network-free **CI tier** (restart-validation of the full event-type set;
agent-no-extra-privilege; local-first no-server backbone) always runs in `cargo test`; the live
three-process **online tier** (three-way membership convergence, signed message exchange,
file share/fetch/verify, live pipe expose/connect, live agent-status delivery) is
`#[ignore]`-gated and run with a documented command. The already-landed Node-API suites
(`join_e2e.rs`, `message_e2e.rs`, `pipe_e2e.rs`, `blob_e2e.rs`, `manager_e2e.rs`) and the two
existing CLI online suites remain the always-green CI backstop for every acceptance criterion
at the lower layers.

**No production code is modified.** Every command, flag, and output line the test drives
already ships and is reconciled to the binary in `docs/getting-started.md`.

---

## 2. Context: what already exists

### 2.1 The full MVP CLI surface (all shipped, reconciled in `docs/getting-started.md`)

| Demo step | Command | Shape relevant to the test |
|---|---|---|
| Identity | `identity create --name <NAME>` / `identity show [--json]` | writes `<home>/identity.json` + `identity.secret`; `show --json` → `{"name","identity_id","device_id"}` |
| Room create | `room create <NAME>` | prints `room_id: blake3:<hex>`, `admin: <id>`; persists genesis to `<home>/rooms.db` |
| Members (offline) | `room members <ROOM_ID> [--json]` | fold of local log; `--json` → `{"room","admin","members":[{identity_id,role,status,is_admin}]}` |
| Invite (human) | `room invite <ROOM_ID> --invitee <ID> [--role member\|agent] [--expires <DUR>]` | admin-only; prints `invite_id:`, `role:`, `expires:`, then `ticket:` + an indented `  roomtkt1…` line |
| Invite (agent) | `agent invite <ROOM_ID> <AGENT_ID> [--expires <DUR>]` | admin-only; thin wrapper over `room invite --role agent`; identical ticket/error codes; prints `role: agent` |
| Join | `room join <TICKET> [--peer <ADDR>]… [--loopback] [--timeout <DUR>]` | one-shot; dials admin, pulls membership sub-DAG, publishes `member.joined`; prints `joined:`, `members: N active`; **fails** if the admin never observes the join |
| Tail (host) | `room tail <ROOM_ID> --accept-joins [--peer …]… [--loopback]` | **long-running**; prints `listening: <ENDPOINT_ID>@<ip:port>` then streams; `--accept-joins` opens provisional bootstrap (admin); a managed `room tail` also **serves the blobs it holds** over the ACL-gated `iroh-blobs` ALPN |
| Send | `room send <ROOM_ID> <MSG> [--peer …]… [--loopback]` | one-shot, offline-first; always `stored: yes`; prints `sent: <event_id>`, `delivered: N …` |
| Tail (offline read) | `room tail <ROOM_ID> --offline [--json] [--limit N]` | **one-shot**, network-free; re-validates + folds from authoritative bytes; renders **all** validated event types in canonical `(lamport,event_id)` order |
| File share | `file share <ROOM_ID> <PATH> [--name …] [--mime …]` | offline; imports to `<home>/blobs/`, recomputes BLAKE3-256, persists `file.shared`; prints `file_id:`, `hash:` |
| File list | `file list <ROOM_ID> [--json]` | offline; provider = `you (local)` / `reference-only` |
| File fetch | `file fetch <ROOM_ID> <FILE_ID> [--out …] [--peer …]… [--timeout <DUR>]` | dials providers over ACL-gated ALPN; independently re-verifies BLAKE3-256; prints `saved:`, `verified:`, `size:`, `provider:` |
| Pipe expose | `pipe expose <ROOM_ID> --tcp 127.0.0.1:<port> --allow <ID>… [--loopback]` | **long-running**; ⚠ SECURITY lines + rejects → **stderr**; prints `listening:` + `pipe_id: <32-hex>` to stdout; authors a `pipe.opened` event |
| Pipe connect | `pipe connect <ROOM_ID> <PIPE_ID> --local <port> [--peer …]… [--loopback]` | **long-running**; binds `127.0.0.1:<port>`, prints `forwarding: 127.0.0.1:<port> -> pipe <id>` |
| Pipe close | `pipe close <PIPE_ID> [--room <ROOM_ID>]` | one-shot; authors a `pipe.closed` event |
| Agent status | `agent status <ROOM_ID> <STATUS> [--message …] [--progress 0..100] [--artifact <FILE_ID>]… [--peer …]… [--loopback]` | one-shot, offline-first (same contract as `room send`); posts a signed `agent.status`; **not role-gated** — any active member may post |

Key enablers for a CI-safe three-process test (identical to the sibling suites):

- **`--loopback`** (hidden `#[arg(long, hide = true)]`) on every online command routes through
  `NetMode::Loopback` → `Endpoint::builder(presets::Minimal)` with `RelayMode::Disabled`
  (`crates/iroh-rooms-net/src/transport.rs`). No relay, no n0 discovery — pure loopback QUIC
  over `127.0.0.1`. This is the literal code-level proof of AC1.
- **`--peer <ENDPOINT_ID>[@<ip:port>]`** supplies an explicit dial address; the long-running host
  commands print their dialable address on a `listening:` line the harness parses and threads in.
- **On-disk isolation**: each participant is a distinct `--data-dir <PATH>` → distinct
  `<home>/rooms.db` + `<home>/blobs/`. **Restart** is simply a fresh process against the same dir.

### 2.2 Landed backstops (already green in CI at the lower layers)

- **Node-API e2e** (`crates/iroh-rooms-net/tests/`): `join_e2e.rs` (valid/agent join converges;
  bad-secret / expired-invite rejection, member- and agent-role), `message_e2e.rs` (signed
  round-trip), `pipe_e2e.rs` (P1 authorized round-trip, P2/P3 unauthorized/non-member denial +
  audit sink), `blob_e2e.rs` (authorized fetch+verify, hash-mismatch reject, connect-gate deny,
  per-hash deny, offline→unavailable), `manager_e2e.rs` (snapshot-admission live flip).
- **CLI online e2e** (`crates/iroh-rooms-cli/tests/`): `two_peer_e2e.rs` (two-human membership /
  message-restart / pipe / file fetch), `agent_e2e.rs` (agent join+converge; agent status
  delivers online + persists on the peer).
- **CLI offline** (`tail_cli.rs`, `room_cli.rs`, `agent_cli.rs`, `file_cli.rs`, `invite_cli.rs`):
  per-event-type offline tail rendering, roster JSON, agent-invite ACs, file gates, and — crucial
  for this spec's CI tier — the pattern of **seeding a full event-type chain via the pure core
  builders** and reading it back with `room tail --offline --json`.

This spec **reuses those patterns and treats the backstops as the always-green lower-layer
coverage**; it does not recreate them. Its unique contribution is the *unified three-party
product flow* plus the two strengthened assertions in §1.

### 2.3 The pure core builders (the CI-tier seeding tool)

`iroh-rooms-core::event` exports deterministic, clock-/RNG-free assemblers for **every** MVP
event type — `build_room_created`, `build_member_invited`, `build_member_joined`,
`build_member_left`, `build_member_removed`, `build_message_text`, `build_file_shared`,
`build_agent_status`, `build_pipe_opened`, `build_pipe_closed` — each golden-tested. `tail_cli.rs`
and `two_peer_e2e.rs::every_provider_refused_is_peer_unauthorized` already demonstrate seeding a
validated chain directly into a `rooms.db` via `EventStore::insert` after
`validate_wire_bytes`. This lets the CI tier assemble a **maximal-diversity log covering all ten
event types deterministically and network-free**, then prove it re-validates and folds
byte-stably across a cold restart — the strongest possible form of AC2, always green in CI.

### 2.4 The demo transcript already exists (`docs/getting-started.md`)

`docs/getting-started.md` is the human-readable transcript of the exact two-humans-plus-one-agent
demo, reconciled to the binary and machine-checked by `crates/iroh-rooms-cli/tests/docs_conformance.rs`.
It is the "documented … transcript" half of this issue's Test Plan; the automated suite is the
executable half. The two are complementary and must stay consistent (§8).

### 2.5 Constraints from repository memory (load-bearing)

- **verify.sh is the real CI gate** (`[[verify-sh-is-the-real-ci-gate]]`): the new file must pass
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (pedantic), and `cargo test --workspace --all-targets --all-features`. The always-CI tier must
  therefore be loopback-free-and-deterministic; the online tier must be `#[ignore]`-gated so it
  can never flake the merge gate.
- **CLI has no tracing subscriber** (`[[cli-has-no-tracing-subscriber]]`): audit output is dropped
  unless a command installs an explicit stderr sink. The observable, CLI-native denial signal for
  the pipe leg is the `pipe expose` stderr audit sink (IR-0108): `pipe.connect.rejected:<cause>`.
- **FsStore exclusive lock needs shutdown** (`[[fsstore-exclusive-lock-needs-shutdown]]`) and
  **blob add_path requires absolute path** (`[[blob-add-path-requires-absolute]]`): a serving
  `room tail` holds the blob store's exclusive on-disk lock; a concurrent `file share` / `file
  list` on the **same home** must run before that session starts (or after it stops). This dictates
  the demo choreography (§7.3): Alice `file share`s **offline first**, then starts the serving
  `room tail`. The suite passes absolute fixture paths (`TempDir` paths are absolute).
- **Membership snapshot ignores content events** (`[[membership-snapshot-ignores-content-events]]`)
  and **member-message ancestor-view gate** (`[[member-message-ancestor-view-gate]]`): convergence
  is asserted over `Member*` events via `room members --json`, never inferred from content events;
  restart-validation asserts *presence + count + byte-stability* of the offline projection, not a
  fold-snapshot equality over content events (which would be vacuous).
- **ADW worktree stale main ref** (`[[adw-worktree-stale-main-ref]]`): diff against `HEAD`, not
  `main`, when isolating this change.

---

## 3. Goals / Non-goals

### Goals

1. One cohesive, product-level integration test proving **PRD §6's full ten-step demo** with the
   full cast — **two humans and one agent** — driven through the **real `iroh-rooms` binary**
   across three isolated on-disk homes, with **no central application server** anywhere in the loop.
2. Cover all five issue acceptance criteria, each mapped to a named test function.
3. Run the reliable subset in CI unconditionally; gate the live three-process subset behind
   `#[ignore]` with a documented command — never let this suite make `verify.sh` flaky.
4. Prove **"all events validate after restart" across every MVP event type** (not just messages),
   at full strength in the deterministic CI tier and confirmed end-to-end in the online tier.
5. Prove **"agent posts status but gains no extra privilege"** with both a positive (status posts)
   and a negative (admin-only action refused) assertion.
6. No external tool dependencies (no `python3`, no `curl`): the pipe target and traffic client are
   in-test loopback TCP, mirroring `pipe_e2e.rs` / `two_peer_e2e.rs`.
7. Self-documenting: module docs carry the demo-step → AC → test map, the tier of each test, the
   exact gated-run command, and the no-server invariant note.

### Non-goals

- **Real-NAT / multi-machine connectivity.** That is Gate A (`spike-nat`, IR-0012); the canonical
  test runs single-host over loopback (the PRD demo path). A multi-machine variant stays optional
  prose in `docs/getting-started.md`.
- **Re-testing lower-layer correctness** (signature validation, CBOR canonicality, fold
  determinism, sync windowing, blob ACL matrix, ticket codec). Those have dedicated conformance
  suites and Node-API e2e backstops; this test asserts the *integrated product flow*.
- **Duplicating the sibling suites.** `two_peer_e2e.rs` and `agent_e2e.rs` keep their granular
  per-AC oracles. This suite is the *unified three-party* flow, not a copy of either.
- **Modifying any production code, CLI surface, event schema, or migration.** Everything the test
  needs already ships. If binary output drifts from an assumption, the **test** is fixed, not the
  binary (the binary is the source of truth per `docs/getting-started.md`).

---

## 4. Owning module & new files

| Path | Kind | Purpose |
|---|---|---|
| `crates/iroh-rooms-cli/tests/full_demo_e2e.rs` | **new** integration test | The primary deliverable: the three-participant full-demo test + the child-process harness + fixtures. |
| `crates/iroh-rooms-cli/Cargo.toml` | possible edit (`[dev-dependencies]` only) | Ensure `tokio` (rt-multi-thread + macros + time + net + io-util) is a dev-dep for the in-test loopback echo target / TCP client used by the pipe tier. Already present for `two_peer_e2e.rs`; verify no new line is needed. `assert_cmd`, `predicates`, `tempfile`, `serde_json`, `hex`, `iroh-rooms-core` are already available to the test target. |
| `docs/getting-started.md` (optional) | doc edit | One line under the Status section pointing at #34's gated-run command, naming this suite as the executable transcript of the full demo. Non-blocking. |

No new production modules. No new crate. No changes under `src/`.

> **Harness-duplication note.** The two existing online suites (`two_peer_e2e.rs`, `agent_e2e.rs`)
> each **port the `ChildSession` harness and fixture helpers verbatim** rather than sharing a
> module — the established repository convention for `tests/*.rs`. This spec follows that
> convention (self-contained file) to avoid coupling three suites through a shared `tests/common`
> module that Cargo would compile into each. **OQ-1** revisits extracting a shared
> `tests/common/mod.rs` as a follow-up if the triplication becomes a maintenance cost.

---

## 5. Design decisions

**D1 — Primary layer is the CLI process, not the Node API.** The issue is a Phase 1B *product*
deliverable ("Integration test for human + agent workflow"); scope names room create/invite/join,
message exchange, file share/fetch/verify, live pipe, agent status, and restart persistence — the
user-facing demo. The Node layer is already covered by the net e2e suites; #34's unique value is
the binary-level, three-home, full-cast chain. → New suite lives in `crates/iroh-rooms-cli/tests/`.

**D2 — Three isolated homes: Alice (admin, human), Bob (human), Agent.** Each is a distinct
`--data-dir` `TempDir` → distinct `rooms.db` + `blobs/`. This is scope bullet "two human peers plus
one agent identity" made literal, and it is what makes "no central server" observable (only three
`iroh-rooms` children + one in-test loopback echo target — the *service being exposed*, not
infrastructure — are ever spawned).

**D3 — Loopback + explicit `--peer`, never real network.** All online commands run with `--loopback`
(`RelayMode::Disabled`, `presets::Minimal`), wired by parsing each host's `listening:` address into
the peers' `--peer`. Hermetic (no relay, DNS, or discovery), CI-eligible, and the literal proof of
AC1.

**D4 — Tier by reliability; gate the live tier.** The Test Plan authorizes exactly this
("Automated end-to-end test where feasible; otherwise documented local test command and transcript
requirement"). Two tiers:

- **CI tier (always run, `#[test]`):** deterministic, no live cross-process networking — the full
  restart-validation over all event types (AC2), the agent-no-extra-privilege pair (AC5 negative +
  positive-offline), and the local-first no-server backbone (AC1). All offline reads/writes and
  builder-seeded logs; they cannot flake.
- **Online tier (`#[ignore]`, documented command):** the live three-process demo — three-way
  convergence, message exchange, file fetch+verify, live pipe, live agent-status delivery. Marked
  `#[ignore]` so `verify.sh` stays green regardless of the runner's networking; run locally (and in
  an optional non-blocking CI job) with the documented command. Every AC also has a green-in-CI
  Node-layer and/or CLI-offline backstop, so gating loses **no** guaranteed coverage.

**D5 — AC2 ("all events validate after restart") is proven at full strength in the CI tier, then
confirmed online.** Two complementary tests:

- **`all_event_types_validate_after_restart` (CI):** seed a single `rooms.db` with **one event of
  every MVP type** via the pure builders (§2.3): `room.created` (Alice), `member.invited` (Bob),
  `member.invited` (Agent, role agent), `member.joined` (Bob), `member.joined` (Agent),
  `message.text`, `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status`, plus `member.left`
  and `member.removed` for departure coverage. Each is `validate_wire_bytes`-checked before
  `insert` (so the seed is itself proof the bytes are valid), the writing store is dropped, and a
  **fresh `iroh-rooms room tail --offline --json` process** reads the log. Assert: (a) every
  expected `event_type` is present in the projection, (b) the projected row **count equals the
  authored count** (nothing silently dropped as invalid on reload — the honest form of "all events
  validate"), and (c) two cold reads are **byte-identical** (fold determinism across restart).
  Deterministic, network-free → always CI.
- **`full_demo_log_validates_after_restart` (`#[ignore]`, online):** after the live demo narrative
  (D6) has run the whole flow, restart **all three** participants and `room tail --offline --json`
  each home. Assert every event type the *networked* flow produced (`member.joined`×2, `message.text`,
  `file.shared`, `pipe.opened`, `pipe.closed`, `agent.status`) is present and byte-stable, and the
  three homes' rosters converge. This proves the same guarantee against genuinely wire-delivered,
  cross-home-synced events — the product-level AC2.

  Grounding: IR-0201's restore-on-open re-validates each persisted `wire` via `validate_wire_bytes`
  on load (a corrupt row is dropped + logged, never a panic); `room tail --offline` re-validates and
  folds purely from the authoritative `(event_id, wire)` rows. So "validates after restart" is
  exactly what the offline projection exercises.

**D6 — A single online narrative test is the primary AC1/AC3/AC4/agent-online oracle.** Unlike the
two-human suite (which favors granular per-AC online tests), #34's headline AC is "**the full demo
completes**" as one flow. The suite's centerpiece is `full_demo_two_humans_one_agent` (`#[ignore]`,
`#[tokio::test]`): the whole PRD §6 demo in causal order (§7.3), asserting each step's success
inline. It is the executable transcript. Granular gated tests (`three_way_membership_converges`,
`authorized_pipe_forwards_bytes_three_party`, `unauthorized_member_pipe_denied`) remain as
narrow oracles so a failure localizes, mirroring the sibling suites' structure.

**D7 — Choreograph around the blob exclusive lock.** A serving `room tail` holds `blobs/`'s
exclusive lock. Therefore Alice **`file share`s offline first** (before any tail), then starts a
single `room tail --accept-joins --loopback` session that simultaneously (a) hosts the provisional
join-bootstrap window for both invitees, (b) serves the already-imported blob over the ACL-gated
ALPN, and (c) receives live messages / agent status. The pipe leg runs **after** that session is
dropped, because `pipe expose` is a separate long-running Alice process and the cleanest,
contention-free arrangement is one long-running Alice session at a time (matching how
`two_peer_e2e.rs` runs its pipe tier against a freshly converged room). See §7.3 for the exact order.

**D8 — AC4 ("pipe access is explicitly authorized") uses the three-party room to get a *free*
denial case.** Alice exposes the loopback echo target allowing **Bob only**. Then: (a) **Bob**
(allow-listed, Active) connects → bytes round-trip (`ping`→`ping`); (b) **the Agent** (Active member
but *not* on the allow-list) connects → denied owner-side by `pipe_connect_allowed`, no bytes
forwarded, and the owner's stderr logs `pipe.connect.rejected:not_allowed` (IR-0108 audit sink) while
the connector's stderr logs `[pipe] denied by the owner`. The three-party cast makes the
authorized-vs-denied contrast a single natural scenario (no fabricated id needed, unlike the
two-human suite's D8). Backstopped by `pipe_e2e::p1`/`p2` + the audit-sink recording test.

**D9 — AC5 ("agent can post status but no implicit extra privilege") = one positive + two
negatives, mostly CI-tier.**

- **Positive (online, in the narrative + backstopped by `agent_e2e.rs`):** the Agent runs
  `agent status <room> running_tests --message … --progress 40 --peer <alice> --loopback` → succeeds,
  `delivered: 1 connected peer(s)`, and persists on Alice as `state=running_tests … role=agent`.
- **Positive (offline, CI):** in a builder-seeded room where the Agent is an active member, the
  Agent's own `agent status` exits 0 with `stored: yes` — posting is gated only by
  `gate_active_member` (spike §7 "any current member"), not by role.
- **Negative — no admin privilege (offline, CI):** from the **Agent's** home, `room invite` /
  `agent invite` is **refused** — the fold shows the Agent is not the room's single immutable admin.
  Assert exit is the Auth category (`3`) and the message names the admin-only requirement, matching
  `invite_cli.rs::invite_non_admin_exits_nonzero_with_actionable_message` /
  `agent_cli.rs::agent_invite_by_non_admin_is_rejected`. The Agent authored no admin-authored event.
- **Negative — least privilege in the fold (offline, CI):** `room members --json` on the Agent's
  converged log shows the Agent as `role: agent` (least-privileged in the `Agent < Member < Admin`
  lattice), never `admin`/`member`.

Together these prove the agent is an ordinary, explicitly-invited principal whose only capability is
"active member" — no implicit extra privilege (spike §3.5; PRD §13.3).

**D10 — In-test loopback echo target + TCP client for the pipe leg (no external tools).** Bind a
tokio TCP echo server on `127.0.0.1:0` as the `--tcp` target; drive the round-trip with an in-process
`TcpStream` to the connector's `--local` port. Exactly the `spawn_echo_server` / `TcpStream` pattern
from `pipe_e2e.rs` and `two_peer_e2e.rs`. Zero `python3`/`curl` dependency (portability + CI).

**D11 — `ChildSession` harness with kill-on-drop and bounded readiness waits.** Long-running commands
(`room tail --accept-joins`, `pipe expose`, `pipe connect`) are not one-shot. Port the landed
`ChildSession` (spawn with `Stdio::piped()` + reader threads draining stdout/stderr into
`Arc<Mutex<String>>`; `wait_for_line`/`wait_for_stderr_line` with a `Duration` budget;
`Drop` → `child.kill()` (SIGKILL) + `wait` + join readers). SIGKILL is the portable, `unsafe`-free
stop the workspace already uses; temp homes are discarded at test end, so a `pipe.opened` lingering
without a matching `pipe.closed` on SIGKILL is irrelevant — and where a clean `pipe.closed` is wanted
(the restart-validation online test), the suite issues an explicit `pipe close` one-shot rather than
relying on signal teardown.

**D12 — Bound every wait; serialize the online tier.** `WAIT = Duration::from_secs(15)` per network
step (matching the sibling suites); the documented run pins `--test-threads=1` to avoid port/resource
contention across three-process tests. A rendezvous bug is a fast assertion error, never a CI hang.

**D13 — Convergence + restart assertions via `--json`, over `Member*` events only.** Three-way
convergence is asserted by parsing `room members --json` on **all three** homes and comparing
order-independent sets; restart is asserted over the `room tail --offline --json` projection. Both use
stable JSON (IR-0106), avoiding brittle text parsing and honoring the "membership derives from
`Member*` events" memory (content events are asserted by presence/count, not fold-snapshot equality).

---

## 6. Test architecture

### 6.1 The `ChildSession` harness (ported, self-contained)

```rust
/// A spawned long-running `iroh-rooms` session (room tail / pipe expose / pipe connect).
/// Reader threads drain stdout+stderr into shared buffers; Drop kills the child (kill-on-drop),
/// so no orphan survives a panic or early return. Every wait is timeout-bounded.
struct ChildSession {
    child: std::process::Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}
impl ChildSession {
    fn spawn(data_dir: &Path, args: &[&str]) -> ChildSession;      // piped stdio + reader threads
    fn wait_for_line(&self, needle: &str, t: Duration) -> Result<String, String>;        // stdout
    fn wait_for_stderr_line(&self, needle: &str, t: Duration) -> Result<String, String>; // stderr (AC4 denial)
    fn stdout_snapshot(&self) -> String;
    fn stderr_snapshot(&self) -> String;
}
impl Drop for ChildSession { /* child.kill(); child.wait(); join readers */ }
```

Port verbatim from `agent_e2e.rs` / `two_peer_e2e.rs` (both carry an identical copy). Include the two
harness self-guard unit tests (`child_session_captures_output`, `child_session_captures_stderr`) so a
harness regression is caught in the CI tier independently of the networked tests.

### 6.2 Shared fixture helpers (ported)

```rust
fn bin_path() -> PathBuf;                                   // assert_cmd::cargo::cargo_bin("iroh-rooms")
fn one_shot(dir, &[args]) -> Output;                        // env_remove(IROH_ROOMS_HOME) + --data-dir
fn extract_field(out, key) -> Option<&str>;                 // "key: value" line parser
fn extract_ticket(out) -> Option<&str>;                     // token on the line after "ticket:"
fn parse_listening(line) -> String;                         // "listening: <addr>" → addr
fn parse_forwarding(line) -> SocketAddr;                    // "forwarding: 127.0.0.1:<port> -> …" → socket
fn identity_create(home, name);                             // identity create --name
fn identity_id(home) -> String;                             // identity show --json → identity_id
fn room_create(home, name) -> String;                       // room create → room_id
fn invite(home, room, invitee_id, role, expires) -> String; // room invite → roomtkt1… ticket
fn agent_invite(home, room, agent_id) -> String;            // agent invite → roomtkt1… ticket (role: agent)
fn members_json(home, room) -> serde_json::Value;           // room members --json
fn roster_set(v) -> BTreeSet<(String,String,String)>;       // {identity_id, role, status} set
fn wait_until_member_status(home, room, id, status, t);     // poll room members --json
fn spawn_echo_server() -> (SocketAddr, Arc<AtomicUsize>);   // in-test tokio loopback echo (pipe target)
fn write_file(dir, name, bytes) -> String;                  // absolute-path fixture writer
fn signing_keys(home) -> (SigningKey, SigningKey);          // reconstruct keys from identity.secret (seeding)
```

All exist verbatim in the sibling suites; port them. `roster_set` order-independence and the parsers
each get a network-free unit test (already written in `two_peer_e2e.rs`) ported into the CI tier.

### 6.3 Per-AC / per-step test functions

| Coverage | Test fn | Tier | Mechanism |
|---|---|---|---|
| **AC1** — completes without central server (backbone) | `full_slice_runs_without_central_server` | **CI** | Three homes; identities; Alice `room create`; `invite(Bob)` + `agent_invite(Agent)` (out-of-band tickets, no server); offline `room send`, `room members --json`, `room tail --offline --json` all exit 0 with only local `--data-dir` stores and no server spawned. |
| **AC1 + AC3 + AC4 + agent-online** — the full demo | `full_demo_two_humans_one_agent` | **`#[ignore]`** | The whole PRD §6 flow in causal order (§7.3): 3 identities → room → 2 invites → Alice serving `room tail --accept-joins` → Bob & Agent join & converge (3 active) → Bob `room send` delivered live → Agent `agent status` delivered live → Bob & Agent `file fetch` + verify → drop tail → Alice `pipe expose --allow Bob` → Bob round-trips, Agent denied → `pipe close`. Asserts each step inline; the executable transcript. |
| **AC2** — all events validate after restart (full type set) | `all_event_types_validate_after_restart` | **CI** | Builder-seed one of **every** event type into a `rooms.db` (validate-then-insert), drop the store, then a **fresh process** `room tail --offline --json` reads it: every `event_type` present, projected count == authored count, two cold reads byte-identical. |
| **AC2** — full demo log validates after restart | `full_demo_log_validates_after_restart` | **`#[ignore]`** | After the online narrative, restart all three homes; `room tail --offline --json` each; assert the wire-delivered event types are present + byte-stable and the three rosters converge. |
| **AC3** — file content hash verifies | (covered inside `full_demo_two_humans_one_agent`) + `blob_e2e.rs` backstop | **`#[ignore]`** | Bob/Agent `file fetch` prints `verified:` == the `hash:` Alice declared at share; saved bytes == original content. |
| **AC4** — pipe access explicitly authorized | `authorized_pipe_forwards_bytes_three_party` + `unauthorized_member_pipe_denied` | **`#[ignore]`** | Converge 3-member room; Alice `pipe expose --allow Bob`; Bob connects → `ping` round-trips; Agent (Active, not allowed) connects → no bytes + owner stderr `pipe.connect.rejected:not_allowed` + connector stderr `[pipe] denied by the owner`. |
| **AC5** — agent posts status, no extra privilege | `agent_posts_status_but_has_no_admin_privilege` | **CI** | Builder-seed a room with the Agent active; Agent `agent status` → exit 0 `stored: yes`; Agent `room invite`/`agent invite` → refused (exit 3, admin-only); `room members --json` shows Agent `role: agent`. |
| harness self-guards | `child_session_captures_output`, `child_session_captures_stderr`, `roster_set_is_order_independent`, parser unit tests | **CI** | Port from the sibling suites so a harness/parse regression is caught network-free. |

`full_demo_two_humans_one_agent` is the human-readable "product slice"; the granular tests localize
failures. AC3's assertion also lives *inside* the narrative (the fetch+verify step), with
`blob_e2e.rs` as the exhaustive green-in-CI backstop — so AC3 is not solely gated.

### 6.4 The "no central server" assertion, concretely

AC1 is a structural/observable property. The suite makes it observable by: (a) never spawning any
relay/broker/server process (only the three `iroh-rooms` children + the in-test loopback echo target,
which is the *service being exposed*); (b) passing `--loopback` → `RelayMode::Disabled` on every
online command, so no relay is even reachable, proven indirectly by the online tier succeeding on
`--peer` direct dials alone; (c) proving the offline backbone works with the machine's network
effectively unused; and (d) a module doc comment citing `transport.rs` (`RelayMode::Disabled`) as the
code-level guarantee.

---

## 7. The full-demo choreography (`full_demo_two_humans_one_agent`)

### 7.1 Cast & homes

`alice_home` (admin, human), `bob_home` (human), `agent_home` (agent). Each a `TempDir` →
`--data-dir`.

### 7.2 One-time setup

1. `identity_create` in all three; capture `bob_id`, `agent_id` via `identity show --json`.
2. Alice `room create "Full Demo Room"` → `room`.
3. Alice `invite(room, bob_id, "member", Some("24h"))` → `bob_ticket`;
   `agent_invite(room, agent_id)` → `agent_ticket` (assert `role: agent`).
4. **Alice `file share <room> <abs_path>` OFFLINE** (before any serving tail — blob-lock, D7).
   Capture `file_id` and `declared_hash`. This event is now in the log and the blob is imported.

### 7.3 The live flow (causal order; blob-lock-safe)

5. **Alice starts one serving host session:** `ChildSession::spawn(alice_home, ["room","tail",
   &room,"--accept-joins","--loopback"])`; `wait_for_line("listening:")` → `alice_addr`. This session
   hosts join-bootstrap, serves the shared blob, and receives live content.
6. **Membership (AC1 wire proof):** Bob `room join <bob_ticket> --peer <alice_addr> --loopback`
   (one-shot, assert exit 0 + `members: 2 active`); Agent `room join <agent_ticket> --peer
   <alice_addr> --loopback` (assert exit 0 + `members: 3 active`).
   `wait_until_member_status(alice_home, …)` for Bob=active and Agent=active. All three
   `room members --json` rosters must be set-equal: `{Alice admin/active, Bob member/active,
   Agent agent/active}` (**three-way convergence**).
7. **Signed message exchange:** Bob `room send <room> "prototype is up" --peer <alice_addr>
   --loopback` → assert `stored: yes` + `delivered: 1 connected peer(s)` (reaches Alice's live
   tail). Optionally Agent also sends.
8. **Agent status (AC5 positive, online):** Agent `agent status <room> running_tests --message
   "suite in progress" --progress 40 --peer <alice_addr> --loopback --timeout 10s` → assert
   `delivered: 1 connected peer(s)`. Poll Alice's offline `room tail --json` until the
   `agent.status` row appears (receive-side durable persistence, `role=agent`).
9. **File fetch + verify (AC3):** Bob `file fetch <room> <file_id> --out <bob_out_dir> --peer
   <alice_addr> --loopback` → assert `verified:` == `declared_hash`, saved bytes == original.
   Repeat for the Agent (proves the agent, as an ordinary active member, may fetch — no special
   privilege needed or granted). Both dial Alice's serving tail.
10. **Stop Alice's serving session** (`drop(alice_tail)`) — frees the blob lock and leaves one
    long-running Alice process free for the pipe leg.
11. **Live pipe (AC4):** `spawn_echo_server()` → `echo_addr`. Alice `pipe expose <room> --tcp
    <echo_addr> --allow <bob_id> --loopback` (`ChildSession`); parse `pipe_id` + expose
    `listening:`. **Authorized:** Bob `pipe connect <room> <pipe_id> --local 0 --peer <alice_addr>
    --loopback`; parse `forwarding:`; `TcpStream` write `ping`, `read_exact` → `ping`; assert
    `echo_count >= 1`. **Unauthorized:** the Agent (Active, not allow-listed) `pipe connect …`;
    assert no `ping` echoes back, Alice's stderr shows `pipe.connect.rejected:not_allowed`, the
    Agent's stderr shows `[pipe] denied by the owner`, `echo_count` unchanged.
12. **Clean pipe close:** Alice `pipe close <pipe_id>` (one-shot) → authors `pipe.closed` (so the
    log carries a matched open/close pair for the restart-validation online test).

### 7.4 Restart validation (AC2, online form)

13. With all sessions stopped, run `room tail --offline --json` on **each** of the three homes in a
    fresh process. Assert every wire-delivered event type is present, the projection is byte-stable
    across two cold reads per home, and the three rosters converge. (`full_demo_log_validates_after_restart`
    may share this fixture or re-run the flow; keep it a distinct `#[ignore]` fn for failure
    localization.)

> The narrative asserts inline at each step so a break localizes to a step; it is `#[ignore]`
> `#[tokio::test(flavor = "multi_thread")]` because of the in-test echo server + TCP client and the
> three live child sessions.

---

## 8. CI integration, reliability & the transcript requirement

- **`scripts/verify.sh` / the CI workflow are unchanged.** `cargo test --workspace --all-targets
  --all-features` runs the **CI tier** (non-ignored) automatically; the `#[ignore]` online tier is
  skipped, so CI stays deterministic.
- **Documented gated command** (the Test Plan's "documented local test command"):

  ```bash
  # Full two-humans-plus-one-agent demo proof (membership convergence, message, file fetch+verify,
  # live pipe, agent status, restart validation). Loopback only; no relay, no external tools.
  # Serialize to avoid port/resource contention across three-process tests.
  cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1
  ```

- **Transcript requirement (Test Plan).** The issue asks for "documented local test command **and
  transcript requirement**" for anything not fully automated. This spec satisfies it two ways:
  1. **Executable transcript:** `full_demo_two_humans_one_agent` runs the whole demo through the
     binary; its inline assertions on each printed line (`members: N active`, `delivered: 1 …`,
     `verified: …`, `pipe.connect.rejected:not_allowed`, …) *are* the transcript-of-record, and its
     module docs carry the demo-step → assertion map.
  2. **Human transcript:** `docs/getting-started.md` is the reconciled, machine-checked
     (`docs_conformance.rs`) prose transcript of the identical demo. The spec requires the online
     test's asserted output lines to stay consistent with that doc; if they drift, fix the **test's
     parser/expectation** and reconcile the doc, never the binary (source-of-truth rule).
     Optionally, the narrative test may write a captured transcript file under a
     `--data-dir`-adjacent temp path for manual inspection — not required for the assertions.
- **Optional non-blocking CI job (recommended follow-up, not required):** a separate workflow job
  running the `--ignored` tier on `ubuntu-latest` with `continue-on-error: true` (or nightly),
  surfacing flakes without gating merges. If it proves stable, consider promoting
  `three_way_membership_converges` out of `#[ignore]` (start conservative).
- **Every wait is bounded** (`WAIT = 15 s`); a wiring failure is a fast assertion error, never a hang.
- **Backstop invariance:** all lower-layer e2e suites and both sibling CLI online suites stay green
  in CI and cover each AC at the Node/CLI layers, so gating this suite's online tier removes **no**
  guaranteed CI coverage — it adds the unified three-party product proof on top.

---

## 9. Acceptance criteria mapping

| Issue AC | Proven by (CLI, this suite) | Tier | Lower-layer backstop (green in CI) |
|---|---|---|---|
| Full demo completes without central application server | `full_slice_runs_without_central_server` (offline backbone) + `full_demo_two_humans_one_agent` with `--loopback` (`RelayMode::Disabled`) on every online step; harness spawns no server | CI + `#[ignore]` | All net e2e suites run `NetMode::Loopback`; `two_peer_e2e.rs`, `agent_e2e.rs` |
| All events validate after restart | `all_event_types_validate_after_restart` (every event type, builder-seeded, count + byte-stable) + `full_demo_log_validates_after_restart` (wire-delivered set) | CI + `#[ignore]` | IR-0201 `sync_restart.rs`, store `rebuild()` determinism, `tail_cli.rs` restart tests |
| File content hash verifies | fetch+verify step inside `full_demo_two_humans_one_agent` (`verified:` == declared `hash:`, saved==original) | `#[ignore]` | `blob_e2e.rs` (authorized fetch+verify, hash-mismatch reject), `two_peer_e2e.rs::authorized_file_fetch_…` |
| Pipe access is explicitly authorized | `authorized_pipe_forwards_bytes_three_party` (Bob round-trips) + `unauthorized_member_pipe_denied` (Agent denied, owner stderr) | `#[ignore]` | `pipe_e2e.rs` P1/P2 + audit-sink recording; `two_peer_e2e.rs` pipe tier |
| Agent can post status but has no implicit extra privilege | `agent_posts_status_but_has_no_admin_privilege` (posts ok; admin-only invite refused exit 3; role=agent) + online status delivery in the narrative | CI + `#[ignore]` | `agent_cli.rs` (invite-by-non-admin rejected, status ACs), `agent_e2e.rs::agent_status_delivers_online_and_persists_on_peer` |

Every AC has **both** a product-level (CLI) assertion in this suite and a green-in-CI lower-layer
backstop, so no criterion depends solely on a gated test. AC2, AC3, and AC5's positive/negative all
have an always-CI form.

---

## 10. Risks & mitigations

| # | Risk | Likelihood | Mitigation |
|---|---|---|---|
| R1 | Three-process loopback QUIC rendezvous is flakier than two-process on some CI runners (timing, ephemeral ports, three homes). | **Medium-High** (`risk/high` label) | Online tier is `#[ignore]` by default; Node-layer + sibling CLI backstops stay green; bounded 15 s waits fail fast; documented `--test-threads=1`; poll rosters to absorb async persistence windows. |
| R2 | Long-running child processes orphan on panic/early-return, leaking ports (three sessions now). | Medium | `ChildSession::Drop` kills + waits each child; reader threads drain pipes so children don't block; temp homes discarded. |
| R3 | Blob exclusive-lock contention if Alice serves a `room tail` while a same-home `file share`/`file list` runs. | Medium | Choreography (D7/§7.2–7.3): `file share` runs **offline before** the serving tail; no concurrent same-home blob command while the tail is up. |
| R4 | Sequential two-invitee join-bootstrap against one Alice session races (Bob and Agent both provisional). | Medium | Both joins target the same live `--accept-joins` session; join is one-shot and returns only after the admin observes it; `wait_until_member_status` polls each to `active` before asserting convergence. |
| R5 | Restart-validation "all events validate" is asserted too weakly (a dropped invalid event looks like success). | Medium | Assert **projected row count == authored count** (nothing silently dropped) *and* byte-stability across two cold reads, not merely "some rows present". Builder seed makes the authored count exact. |
| R6 | `pipe.closed` absent from the log if the pipe session is SIGKILLed (Drop), weakening the open/close restart coverage. | Low | Issue an explicit `pipe close <pipe_id>` one-shot (§7.3 step 12) rather than relying on signal teardown; the CI-tier restart test also builder-seeds a `pipe.closed`. |
| R7 | Streamed-stdout parsing (`listening:`/`pipe_id:`/`forwarding:`) races the child's output. | Medium | `wait_for_line` polls a continuously-drained shared buffer with a timeout; parse only after the readiness line appears. |
| R8 | Clippy pedantic failures in a large test file (`too_many_lines`, `unwrap`, `must_use`). | Low | Mirror the sibling suites' style (they pass pedantic); `#[allow(clippy::too_many_lines)]` on the linear narrative fn with a justifying comment (as `agent_e2e.rs`/`two_peer_e2e.rs` already do); run pedantic clippy before finishing. |
| R9 | Binary path resolution differs under `std::process::Command` vs `assert_cmd`. | Low | Resolve once via `assert_cmd::cargo::cargo_bin("iroh-rooms")` and pass the absolute path to `std::process::Command` (as the sibling suites do). |
| R10 | Exact non-admin invite exit code / message drifts from the assumed `insufficient_role`/admin-only shape. | Low | Assert against the *actual* binary output (the sibling `invite_cli.rs`/`agent_cli.rs` non-admin tests are the source of truth for the code/exit; mirror them). Binary is source of truth. |
| R11 | Wall-clock of the full narrative under one 15 s-per-step budget is long. | Low | Steps are a few seconds each under the ceiling; whole `--ignored` suite target < ~90 s; keep granular tests independent so they can run selectively. |
| R12 | Triplicated harness across three CLI suites drifts. | Low | Port verbatim; OQ-1 tracks extracting a shared `tests/common/mod.rs` follow-up. |

---

## 11. Security, privacy, observability, performance

- **Security invariants under test (the product's most safety-critical behaviors):** authorized-only
  pipe forwarding + owner-visible denial (AC4); content-hash verification on fetch (AC3); key-bound
  invites (Bob and the Agent can each only join with their own ticket); the agent as an ordinary
  least-privileged principal with **no** admin capability (AC5). Exercised at the binary boundary
  with the full three-party cast.
- **Secret hygiene:** the suite passes tickets (capability secrets) opaquely and never logs
  `identity.secret`. Where it reconstructs signing keys for builder-seeding (CI tier), it reads the
  seeds only to build events, never prints them. A spot check (ported from `two_peer_e2e.rs`) that
  the ticket contains no raw secret seed may be included.
- **Observability:** AC4's denial relies on the IR-0108 `pipe expose` **stderr** audit sink
  (`pipe.connect.rejected:<cause>`), since the CLI installs no tracing subscriber
  (`[[cli-has-no-tracing-subscriber]]`). The suite asserts the stderr signal, not tracing logs.
- **Privacy:** loopback-only, `RelayMode::Disabled` — no traffic leaves the host. The provisional
  join-bootstrap window discloses only the secret-free membership sub-DAG (IR-0104), which the test
  does not probe beyond the intended two joins.
- **Performance:** CI tier is sub-second (offline reads/writes + builder seeding). The `--ignored`
  narrative targets < ~90 s wall clock (each of the ~12 network steps a few seconds under a 15 s
  ceiling); DX metric context: PRD §17.2 targets first two-peer room < 3 min and first pipe < 5 min —
  the automated flow is well inside those and demonstrates them.

---

## 12. Rollout / rollback

- **Rollout:** purely additive — one new test file (+ possibly one `[dev-dependencies]` line if
  `tokio` is not already dev-available to this target). No production code, CLI surface, schema, or
  migration. Landing it cannot change runtime behavior.
- **Rollback:** delete `full_demo_e2e.rs` (and revert the `Cargo.toml` line if added). Zero blast
  radius on shipped crates.
- **Docs reconciliation:** if the binary's exact output drifts from an assumption, fix the **test**
  (parser/expectation) and reconcile `docs/getting-started.md` — never the binary
  (`docs/getting-started.md` source-of-truth rule; `docs_conformance.rs` guards the doc).

---

## 13. Open questions

- **OQ-1 — Extract a shared `tests/common/mod.rs`?** Three CLI online suites now triplicate
  `ChildSession` + fixtures. **Recommendation:** ship #34 self-contained (match convention), then
  file a follow-up to dedupe all three into a shared test-support module if maintenance cost grows.
- **OQ-2 — One narrative test vs. fully granular per-AC online tests?** The issue's headline AC is
  "the full demo completes," which argues for the single narrative as primary. **Recommendation:**
  keep the narrative as the primary online oracle *and* the two/three narrow gated tests
  (convergence, pipe authorized/denied) for failure localization; put the always-CI forms of
  AC2/AC5 in the CI tier so they never depend on the gated flow.
- **OQ-3 — Should the Agent also fetch the file (vs. only Bob)?** Having the Agent fetch reinforces
  "agent is an ordinary active member with no special privilege" and adds no cost.
  **Recommendation:** yes — both humans-and-agent fetch, but assert `verified:` for at least Bob and
  the Agent.
- **OQ-4 — Which peer receives the live message / status?** Alice's serving `--accept-joins` tail is
  the natural single online receiver. **Recommendation:** send/post to Alice's live session (asserts
  `delivered: 1`), matching `agent_e2e.rs`; a full N-way fan-out (every peer tailing) is heavier and
  not required by the ACs.
- **OQ-5 — Promote any online test to CI later?** If three-process loopback proves reliable on the
  runner. **Recommendation:** ship gated; add the optional non-blocking job; measure; then decide.
- **OQ-6 — SIGTERM vs SIGKILL on drop.** SIGKILL is what the sibling suites use (portable, no
  `unsafe`); it means no `pipe.closed{owner_exit}` on teardown. **Recommendation:** SIGKILL-only +
  an explicit `pipe close` where a clean close event is needed (R6). Implement SIGTERM only if a
  future test asserts `owner_exit`.

---

## 14. Assumptions

- **A1** — The hidden `--loopback` flag and `--peer` addressing behave as documented on the shipped
  binary (verified in source and exercised by `two_peer_e2e.rs` / `agent_e2e.rs`), enabling a
  hermetic three-process test. Any drift is fixed in the test.
- **A2** — `room join` exits non-zero if the admin never observes the join, so a successful join
  implies admin persistence; the test still polls `room members --json` to absorb the tail child's
  async persistence window.
- **A3** — A single Alice `room tail --accept-joins --loopback` session both hosts provisional
  join-bootstrap **and** serves the blobs Alice holds (IR-0104 + IR-0204), so one session covers
  membership bootstrap, message/status receipt, and blob serving. If serving requires a plain (non
  `--accept-joins`) tail in practice, split into two sequential Alice sessions (invites/joins first,
  then a serving tail for fetch) — a choreography detail, not an AC change.
- **A4** — A serving `room tail` holds `blobs/`'s exclusive lock, so Alice `file share`s **offline
  before** starting it (`[[fsstore-exclusive-lock-needs-shutdown]]`). `TempDir` paths are absolute,
  satisfying `[[blob-add-path-requires-absolute]]`.
- **A5** — `room members --json` and `room tail --offline --json` emit stable, parseable JSON
  (IR-0106) suitable for set-equality, presence/count, and body/field assertions; the offline read
  re-validates each wire on load and emits only validated rows (IR-0201).
- **A6** — Every MVP event type has a pure `build_*` core builder usable to seed a full-diversity log
  network-free (verified: `genesis`/`invite`/`join`/`left`/`removed`/`message`/`file`/`status`/`pipe`
  modules under `iroh-rooms-core::event`).
- **A7** — `pipe expose` prints `pipe_id:` + `listening:` to stdout and `pipe.connect.rejected:<cause>`
  to stderr (IR-0108 audit sink); `pipe connect` binds `127.0.0.1:<port>` (OS-assigned for `--local 0`)
  on a `forwarding:` line and prints `[pipe] denied by the owner …` to stderr on an owner denial. The
  allow-list is enforced owner-side, so an Active-but-not-allowed member (the Agent) is the canonical
  authorized-denial case.
- **A8** — A non-admin `room invite` / `agent invite` is refused with an Auth-category exit (`3`) and
  an admin-only message (verified by the landed non-admin rejection tests); the test asserts the
  binary's actual code/exit.
- **A9** — The dev target is macOS/Linux (getting-started prerequisites); Windows child-signal
  portability is out of scope.
- **A10** — `tokio` is available to the CLI test target (it is a normal dependency of
  `iroh-rooms-cli` and already used by `two_peer_e2e.rs`), so the pipe leg's in-test server/client
  add no new dependency resolution.

---

## 15. Summary of deliverables

1. **`crates/iroh-rooms-cli/tests/full_demo_e2e.rs`** — the three-participant full-demo integration
   test: a ported `ChildSession` harness + fixtures, a **CI tier** (`full_slice_runs_without_central_server`,
   `all_event_types_validate_after_restart`, `agent_posts_status_but_has_no_admin_privilege`, harness
   self-guards) and an **`#[ignore]` online tier** (`full_demo_two_humans_one_agent`,
   `three_way_membership_converges`, `authorized_pipe_forwards_bytes_three_party`,
   `unauthorized_member_pipe_denied`, `full_demo_log_validates_after_restart`), with module docs
   carrying the demo-step → AC → test map, the tier of each test, and the gated-run command.
2. **`crates/iroh-rooms-cli/Cargo.toml`** — verify/keep the `tokio` `[dev-dependencies]` line
   (likely already present).
3. **Documented gated command** in the module docs (and optionally `docs/getting-started.md` /
   `README.md`): `cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1`.
4. **Transcript satisfaction:** the online narrative is the executable transcript; `docs/getting-started.md`
   is the reconciled human transcript (guarded by `docs_conformance.rs`) — kept consistent.
5. **No production code changes.** `verify.sh` runs the deterministic CI tier automatically; the live
   tier is gated and backed by the existing green Node-layer and sibling CLI e2e suites.
```
