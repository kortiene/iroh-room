# Spec: Authenticated TCP Pipe expose / connect / close (product completion)

| | |
|---|---|
| **Issue** | #23 — [IR-0108] Implement authenticated TCP pipe expose/connect/close |
| **Parent** | #2 |
| **Labels** | type/feature, type/security, area/cli, area/pipe, priority/p0, risk/high |
| **Depends on** | #14 (IR-0010, live TCP pipe prototype — **landed**), #21 (IR-0106, offline room-read CLI — **landed**), #12 (IR-0008, membership fold — **landed**) |
| **Traceability** | `PRD.v0.3.md` §9.3 (Live Pipe Plane), §13.2 (Pipe Security Requirements 1–8), §15.7 (Open Live Pipe user journey, AC1–AC8), §16 (CLI Requirements + UX rules). `PHASE-0-SPIKE.md` Event Protocol §7 (`pipe.opened`/`pipe.closed`), Membership & Ordering §5 (connect-accept gate + revocation-on-learn), §8 (rejection taxonomy). Prior spec: [`specs/live-tcp-pipe-path.md`](./live-tcp-pipe-path.md) (the #14 mechanism this completes). |
| **Status** | Planning — spec only. No production code changed by this document. |
| **Type** | Product-completion pass over the landed #14 prototype: CLI-surface reconciliation to the PRD canonical shape, owner-side audit visibility, acceptance-locking tests (incl. a real HTTP-server e2e), and docs alignment. **No new authorization logic.** |

---

## 1. Summary

Issue #23 is the first **product-differentiating** workflow: authenticated TCP pipe
`expose` / `connect` / `close`. The underlying mechanism — the `/iroh-rooms/pipe/1` ALPN,
the two-stage connect-accept gate, TCP↔QUIC splicing to a loopback target, tear-down-on-learn,
and signed `pipe.opened`/`pipe.closed` events — **already landed** under #14 / IR-0010 (see
[`specs/live-tcp-pipe-path.md`](./live-tcp-pipe-path.md)), together with a working
`iroh-rooms pipe expose | connect | close | list` CLI and a net-layer end-to-end suite
(`crates/iroh-rooms-net/tests/pipe_e2e.rs`, P1–P6).

This issue does **not** re-implement that mechanism. It is the productization pass that closes
the gap between the landed prototype and the PRD's canonical, user-facing contract, and locks
every acceptance criterion behind an executable test. Concretely, the audited delta is:

1. **CLI surface reconciliation.** The PRD (§16, §15.7) and this issue's scope specify
   `iroh-rooms pipe close <pipe-id>` — **no room id**. The landed code requires
   `pipe close <ROOM_ID> <PIPE_ID>` and the `expose` command even prints the room-id form as its
   "close it with:" hint. The prior spec's own command table (`live-tcp-pipe-path.md:441`) already
   says `pipe close <PIPE_ID>`, and `docs/getting-started.md:55–58` carries an explicit
   `[reconcile]` note flagging this divergence. #23 reconciles it: `pipe close` takes a bare
   `<PIPE_ID>` and infers the room from the local log.
2. **"Unauthorized connect is rejected and locally logged" is not user-visible today.** The
   rejection **decision** is correct and conformance-tested, and the owner emits a stable
   `pipe.connect.rejected:<cause>` line through the `TracingPipeAudit` sink — but **the CLI installs
   no `tracing` subscriber** (`crates/iroh-rooms-cli/src/` has none), so on the owner's terminal
   nothing prints. The acceptance criterion "rejected **and locally logged**" therefore fails for the
   actual CLI user. #23 makes the owner-side reject/teardown audit visible locally.
3. **Owner-exit guarantee needs firming and honest bounds.** `expose` publishes
   `pipe.closed{owner_exit}` on `Ctrl-C` (SIGINT) today. #23 extends graceful close to SIGTERM,
   and documents the residual: a hard kill (SIGKILL / power loss) leaves the last `pipe.opened`
   with no matching `pipe.closed` on the log; the *forwarding* still stops (owner endpoint dies;
   connector tears down), but the log shows the pipe "open" until an owner/admin `pipe close`.
4. **Security-warning completeness.** The warning must **name the exposed target and the allowed
   member(s)** (§13.2.4 / issue Security Note). Verify and, if needed, tighten wording.
5. **Acceptance test debt.** There is **no CLI-level pipe integration test** (`tests/pipe_cli.rs`
   does not exist), and the net-layer e2e uses a trivial echo server. This issue's Test Plan
   explicitly names *a local HTTP server*. #23 adds a CLI-level integration suite and a
   real-HTTP-server e2e (authorized connect, unauthorized connect, clean close).
6. **Docs reconciliation.** Update `docs/getting-started.md` Step 6 (remove the `[reconcile]`
   note, show `pipe close <PIPE_ID>`), the README pipe bullet, and the docs-conformance test.

The risk label is `risk/high` because pipes expose local services; but the load-bearing
authorization logic is already landed and conformance-tested. The residual risk in *this* issue
is surface/UX correctness (room inference, audit visibility, exit handling), not the trust
boundary — which #23 must not touch except to add tests.

---

## 2. Background & current repository state

**Read before starting:**

- [`specs/live-tcp-pipe-path.md`](./live-tcp-pipe-path.md) — the #14 mechanism, in full. This
  spec assumes it and describes only the delta.
- `PRD.v0.3.md` §9.3, §13.2 (all 8 requirements), §15.7 (AC1–AC8), §16 (CLI + UX rules 1–5).
- `PHASE-0-SPIKE.md` Event Protocol §7 and Membership & Ordering §5.

**Landed and reused unchanged (do not modify the trust logic):**

| Component | Location | Role for #23 |
|---|---|---|
| `build_pipe_opened` / `build_pipe_closed` | `crates/iroh-rooms-core/src/event/pipe.rs` | Byte-exact signed-event assembly. **Unchanged.** |
| `PipeOpened` / `PipeClosed` content + strict §7 validation | `crates/iroh-rooms-core/src/event/content.rs`, `.../validate.rs` | `owner_id == sender_id`, `kind == "tcp"`, non-empty `allowed_members`, `reason ∈ {closed,expired,owner_exit,error}`. **Unchanged.** |
| `pipe_connect_allowed` (pure access predicate) | `crates/iroh-rooms-core/src/membership/access.rs` | The no-default-all authorization decision. **Unchanged.** |
| Pipe transport plane (ALPN, gate, splice, watcher, registry, connector) | `crates/iroh-rooms-net/src/pipe/` | The QUIC↔TCP mechanism. **Unchanged** except as noted in §5. |
| `Node::pipe_expose / pipe_connect / pipe_close / pipe_opened` | `crates/iroh-rooms-net/src/node.rs` | Orchestration entry points the CLI calls. **Unchanged** (a new store query is additive). |
| `PipeAuditSink` / `TracingPipeAudit` / `PipeDenyCause` | `crates/iroh-rooms-net/src/pipe/audit.rs` | Stable reject/teardown vocabulary. #23 adds a *user-visible* surface over it (§5.3). |
| CLI `pipe expose/connect/close/list` | `crates/iroh-rooms-cli/src/pipe.rs`, `cli.rs` | The commands to reconcile and test. **Modified** (§5). |
| `is_loopback_target` | `crates/iroh-rooms-net/src/pipe/…` (re-exported) | Loopback enforcement for `--tcp`. **Unchanged.** |
| Net-layer e2e P1–P6 (echo server) | `crates/iroh-rooms-net/tests/pipe_e2e.rs` | Existing coverage; #23 adds an HTTP variant + CLI-level tests. |

**Gaps this issue closes (verified against the tree, 2026-07-01):**

- `crates/iroh-rooms-cli/src/cli.rs:108–124` — `PipeAction::Close` takes a required `room_id`
  positional. `crates/iroh-rooms-cli/src/pipe.rs:161` prints
  `close it with: iroh-rooms pipe close {room_id} {pipe_hex}`. Both diverge from PRD §16/§15.7.
- `crates/iroh-rooms-cli/src/main.rs` + `src/cli.rs` — **no `tracing` subscriber is installed**,
  so `TracingPipeAudit` / `TracingAudit` output is dropped; owner-side "locally logged" rejections
  never reach the terminal.
- `crates/iroh-rooms-core/src/store/mod.rs` — the query surface is entirely **room-scoped**
  (`by_type(room,…)`, `room_tail(room,…)`, `heads(room)`, `count(room)`). There is **no**
  `SELECT DISTINCT room_id` / global "find the room owning this `pipe_id`" query. `pipe close
  <pipe-id>` needs one.
- `crates/iroh-rooms-cli/tests/` — has `identity_cli`, `invite_cli`, `join_cli`, `message_cli`,
  `room_cli`, `tail_cli`, `docs_conformance`, but **no `pipe_cli.rs`**. The pipe CLI has zero
  integration-test coverage.
- `crates/iroh-rooms-net/tests/pipe_e2e.rs` — uses `spawn_echo_server()`, not an HTTP server; the
  issue Test Plan names a local HTTP server explicitly.
- `expose` handles `Ctrl-C` (SIGINT) only (`tokio::signal::ctrl_c`); SIGTERM (the default `kill`)
  bypasses the graceful `pipe.closed{owner_exit}`.

---

## 3. Goals / Non-goals

### 3.1 Goals

1. `iroh-rooms pipe close <PIPE_ID>` works with **no room id**, inferring the room from the local
   store, matching PRD §16 / §15.7 and this issue's scope.
2. `expose`, `connect`, `close` present the exact canonical argument shapes from the PRD; the
   `expose` "close it with:" hint prints `pipe close <PIPE_ID>`.
3. An unauthorized connect attempt is **rejected and locally logged in a way the owner actually
   sees** on their terminal (AC3 / PRD §13.2.7 / §16.3).
4. The `expose` security warning **names the exposed target and each allowed member** (§13.2.4).
5. A clean `close` emits a signed `pipe.closed{closed}`; the pipe closes on owner **process exit**
   (SIGINT and SIGTERM emit `pipe.closed{owner_exit}`); hard-kill bounds are documented.
6. Every issue acceptance criterion and every PRD §15.7 AC is locked by an executable test,
   including a **real-HTTP-server** end-to-end (authorized connect, unauthorized connect, close)
   and a first **CLI-level** pipe integration suite.
7. Docs (`getting-started.md` Step 6, README, docs-conformance test) reflect the reconciled surface.

### 3.2 Non-goals

- **No change to the authorization model, event schema, gate, or splice logic.** #23 is surface,
  visibility, and tests. Any temptation to "improve" `pipe_connect_allowed`, the §7 validation, or
  the two-stage gate is out of scope and must be rejected in review.
- **Terminal sharing, Unix-socket forwarding, multiplexed pipes, non-TCP transports** — out (PRD
  §9.3 MVP limitation, §13.2.8).
- **Gate-A real-NAT execution** for the pipe ALPN — still owed, inherited from #9/#14, tracked
  separately (`crates/iroh-rooms-net/NOTES.md`); this issue is loopback/CI-deterministic like its
  siblings and does not close Gate A.
- **Multi-hop / relay-through-a-third-peer pipes** — out (both peers online, direct/relayed 1:1).
- **Guaranteed close on power loss / SIGKILL** — impossible without a cooperating peer; bounded and
  documented, not solved (§5.5, §8).

---

## 4. Design decisions

### 4.1 `pipe close <PIPE_ID>` room inference (headline decision)

**Decision.** Make `<ROOM_ID>` **not** a positional on `pipe close`. The command takes a bare
`<PIPE_ID>` and infers the room by scanning the local store for the `pipe.opened` whose
`pipe_id` matches. Add an **optional** `--room <ROOM_ID>` disambiguator.

- The single-room case (the overwhelmingly common one, and the only one the demo produces) resolves
  unambiguously with zero extra typing — exactly the PRD contract.
- If the local `rooms.db` holds the same `pipe_id` in more than one room (astronomically unlikely —
  `pipe_id` is a 16-byte CSPRNG value — but not impossible across imported DBs), fail closed with an
  actionable error naming the candidate rooms and instructing `--room`.
- If no room contains the `pipe_id`, fail with "no such pipe in any local room; run `pipe list
  <ROOM_ID>` or sync first."

**Why not** keep `<ROOM_ID>` required: it contradicts the PRD, the issue scope, the prior spec's own
table, and the getting-started guide's reconcile note. **Why not** make it a silent global scan with
no `--room` escape hatch: multi-room DBs (a power user with several rooms) need a deterministic
override. **Why not** derive room from a local "open pipes I own" side-table: no such table exists;
room state is derived purely from the append-only log by design (README "derived from the
append-only event log"), and adding mutable side-state would break restart-determinism.

`connect` and `expose` keep `<ROOM_ID>` (the PRD shows it for both), so only `close` changes shape.

### 4.2 New store query for room inference

**Decision.** Add one additive, read-only query to `EventStore` (behind the existing `store`
feature), preferred form:

```rust
/// All distinct room ids present in the store, ascending by id.
pub fn room_ids(&self) -> Result<Vec<RoomId>, StoreError>;
```

The CLI then folds each candidate room and looks for the governing `pipe.opened`, reusing the
existing `open_pipe` / `closed_pipe_ids` helpers already in `pipe.rs`. `room_ids()` is a
`SELECT DISTINCT room_id FROM events ORDER BY room_id` — trivial, index-friendly, and generally
useful (a future `iroh-rooms room ls` reuses it). **Alternative considered:** a targeted
`find_pipe_opened(pipe_id) -> Option<(RoomId, PipeOpened)>` that pushes the scan into SQL. Rejected
as the primary API because it special-cases one content type in the store layer; `room_ids()` keeps
the store generic and the pipe logic in the pipe module. (Implementer may add the targeted query as
a private helper if profiling shows the fold-per-room scan is too slow; for MVP-sized rooms it is
not.) The query is additive: **no schema change, no `user_version` bump** (mirrors how
`room_event_ids` was added for sync).

### 4.3 Owner-side audit visibility ("locally logged")

**Decision.** During `pipe expose`, surface owner-side reject/teardown/accept audit lines to
**stderr** via a CLI audit sink, rather than installing a global `tracing_subscriber`.

- Introduce a small CLI-local `PipeAuditSink` implementation (e.g. `StderrPipeAudit`) that writes
  one stable, greppable line per event to **stderr** (keeping stdout script-friendly, PRD §16.5):
  - `pipe.connect.rejected:<cause> peer=<endpoint-short> pipe=<pipe-short>`
  - `pipe.connect.accepted peer=<endpoint-short> pipe=<pipe-short>`
  - `pipe.torndown:<cause> peer=<endpoint-short> pipe=<pipe-short>`
- Wire it into the `Node` the same way `TracingAudit` is wired today (the expose path constructs the
  audit sink; pass the stderr sink instead of / in addition to `TracingAudit`). Confirm `Node::spawn`
  / the pipe runtime accept a `PipeAuditSink` (it already does — `TracingPipeAudit` is the default);
  if the current wiring hard-codes `TracingPipeAudit`, thread the sink through (small, additive).

**Why stderr, not a `tracing` subscriber:** installing a global fmt subscriber would (a) leak
unrelated `iroh`/`tokio` logs into the user's terminal, (b) require `RUST_LOG` plumbing to be useful,
and (c) risk writing to stdout. A purpose-built stderr sink gives the user exactly the security-
relevant lines, deterministically, and is directly assertable in a CLI test. `TracingPipeAudit`
remains for library consumers and structured deployments.

**Optional `--verbose`/`-v`:** default is to print reject/teardown lines (security-relevant, always
useful) and to *suppress* per-accept chatter unless `-v` is passed. Accepts are not security events;
rejects are.

### 4.4 Owner-exit signal handling

**Decision.** Replace the SIGINT-only `wait_for_ctrl_c()` in `expose` with a wait on **both** SIGINT
and SIGTERM (via `tokio::signal::unix::signal(SignalKind::terminate())` on Unix, `ctrl_c()` on all
platforms). Both trigger the existing graceful path: publish `pipe.closed{owner_exit}`, flush
(`FLUSH_GRACE`), shut the node down. SIGKILL and power loss remain unhandleable and are documented
(§5.5, §8). On Windows, keep `ctrl_c()` (SIGTERM has no portable equivalent); the guarantee is
"graceful on catchable termination."

### 4.5 Back-compat

**Decision.** No back-compat shim for `pipe close <ROOM_ID> <PIPE_ID>`. The feature is pre-1.0,
unreleased Phase-0, and the two-positional form was never in the PRD. A stale invocation now fails
clap parsing with a clear usage string. (If the reviewer wants a soft landing, an *optional*
trailing behaviour could accept a second positional and treat it as `--room`, but the recommendation
is the clean single-positional form to match the PRD exactly.)

---

## 5. Implementation steps

Ordered so the tree stays green after each step. Each step is independently reviewable.

### 5.1 WI-1 — `EventStore::room_ids()` (core, `store` feature)

- Add `pub fn room_ids(&self) -> Result<Vec<RoomId>, StoreError>` to
  `crates/iroh-rooms-core/src/store/mod.rs`: `SELECT DISTINCT room_id FROM events ORDER BY room_id`,
  decoding each `room_id` blob into a `RoomId`. Read-only; no schema/`user_version` change.
- Unit tests in the store module: empty store → `[]`; one room → `[r]`; three rooms with interleaved
  inserts → all three, ascending, de-duplicated; survives `rebuild()` (derived-state determinism).

### 5.2 WI-2 — `pipe close <PIPE_ID>` surface + room inference (CLI)

- `crates/iroh-rooms-cli/src/cli.rs`:
  - `PipeAction::Close`: remove the `room_id` positional; keep `pipe_id` positional; add
    `#[arg(long)] room: Option<String>` and the existing `--peer` / `--loopback`.
  - Update the module doc `Surface` block and the `dispatch_pipe` arm to call the new `close`
    signature.
- `crates/iroh-rooms-cli/src/pipe.rs`:
  - Change `close(home, pipe_id_hex, room: Option<&RoomId>, peers, loopback)`:
    1. Parse `pipe_id`.
    2. Resolve the room: if `--room` given, use it; else call a new helper
       `resolve_pipe_room(&store, &pipe_id)` that iterates `store.room_ids()`, folds each, and finds
       the room whose governing `open_pipe(..)` matches `pipe_id`. Zero matches → actionable
       "no such pipe" error; >1 match → actionable "ambiguous, pass --room" error listing candidates.
    3. Proceed exactly as today (membership + owner/admin authz check, bring up node, publish
       `pipe.closed{closed}`, flush, shut down).
  - `expose`: change the printed hint from
    `close it with: iroh-rooms pipe close {room_id} {pipe_hex}` to
    `close it with: iroh-rooms pipe close {pipe_hex}`.
- Note: `connect` and `expose` keep `<ROOM_ID>` unchanged.

### 5.3 WI-3 — owner-side stderr audit sink (CLI, visibility)

- Add `StderrPipeAudit` (implements `iroh_rooms_net::pipe::PipeAuditSink`) in a small CLI module
  (e.g. `crates/iroh-rooms-cli/src/pipe.rs` or a new `pipe_audit.rs`), writing stable lines to
  stderr per §4.3.
- In `expose`, construct and pass this sink into the node's pipe plane (thread a `PipeAuditSink`
  through `Node::spawn` / the pipe runtime if not already parameterized; default remains
  `TracingPipeAudit` for other callers). Gate per-accept lines behind a `-v/--verbose` flag; always
  print rejects/teardowns.
- Confirm the connector side (`connect`) already surfaces `PipeOutcome::Denied` to stderr
  (`pipe.rs:289`) — it does; leave as is, and align its wording to the same vocabulary if helpful.

### 5.4 WI-4 — security-warning completeness (CLI, §13.2.4)

- In `expose`, ensure the warning block explicitly names the target **and** each allowed member.
  Current output prints `target` and one `allow:` line per member but the ⚠ warning line itself is
  generic. Tighten so the warning names the target and enumerates the allowed member ids (short
  form acceptable for the warning; full ids in the labeled `allow:` lines). Keep the two ⚠ lines on
  **stderr** and the machine-readable `room:/target:/label:/allow:/expires_at:` lines on **stdout**
  (matches the existing split and the docs-conformance expectation).

### 5.5 WI-5 — owner-exit on SIGINT **and** SIGTERM (CLI)

- Replace `wait_for_ctrl_c()` with a helper that returns when **either** SIGINT or (on Unix) SIGTERM
  fires. Both run the existing graceful close (`pipe.closed{owner_exit}` → flush → shutdown).
- Document (code comment + docs) that SIGKILL/power-loss cannot emit `pipe.closed`; the connector's
  session still tears down (owner endpoint death) so **forwarding stops**, but the log shows the
  pipe open until an owner/admin `pipe close`. This is the §5 "bounded by reachability" property,
  not a regression.

### 5.6 WI-6 — real-HTTP-server end-to-end test

- Add a test (preferably `crates/iroh-rooms-net/tests/pipe_e2e.rs` as a new case, or a dedicated
  `pipe_http_e2e.rs`) that stands up a **minimal loopback HTTP/1.1 server** (a hand-rolled
  `tokio::net::TcpListener` that replies `HTTP/1.1 200 OK\r\nContent-Length: …\r\n\r\n<body>` to any
  request — no new prod dependency needed) and drives:
  1. **Authorized connect** — an allow-listed Active member connects through the pipe, issues a real
     `GET /`, and reads back the exact HTTP response body. (§15.7 AC1/AC4/AC6.)
  2. **Unauthorized connect** — a non-allowlisted Active member (and, separately, a non-member) is
     denied; **zero** bytes reach the HTTP server (assert the server's accept counter stays 0);
     the owner audit records `pipe.connect.rejected:not_allowed` / `:not_active|unknown_device`.
     (§15.7 AC5.)
  3. **Clean close** — the owner closes; a signed `pipe.closed` appears on the log; a subsequent
     connect fails `closed`. (§15.7 AC7/AC8.)
- Every await is timeout-bounded (mirror the existing `WAIT` discipline in `pipe_e2e.rs`).

### 5.7 WI-7 — CLI-level pipe integration suite (`tests/pipe_cli.rs`)

New file `crates/iroh-rooms-cli/tests/pipe_cli.rs` (using `assert_cmd`, mirroring `room_cli.rs`):

- **Pre-IO validation (offline, fast):**
  - `pipe expose <ROOM> --tcp 8.8.8.8:80 --allow <ID>` → non-zero, "non-loopback" message; nothing
    published.
  - `pipe expose <ROOM> --tcp 127.0.0.1:3000` with no `--allow` → clap "required"; and empty/invalid
    `--allow` → actionable error.
  - `pipe expose` / `connect` / `close` as a **non-member** identity → "not an active member" (for
    `close`, "owner or admin" once membership passes).
  - `pipe close <bad-hex>` → "invalid pipe id".
- **`close <PIPE_ID>` room inference (the headline):** seed a single-room DB containing a
  `pipe.opened` (built via `build_pipe_opened` + persisted), then `pipe close <PIPE_ID>` (no room)
  resolves and publishes `pipe.closed{closed}`; assert via `pipe list`/`room tail --offline` that the
  pipe is now closed. Ambiguous/absent-pipe error paths covered.
- **Security warning + script-friendly split:** assert the ⚠ warning is on **stderr** and names the
  target + allowed member; the `room:/target:/allow:` lines are on **stdout**.
- **Unauthorized-connect local log (AC3):** a two-node CLI-level or net-backed test asserting the
  owner's stderr contains `pipe.connect.rejected:not_allowed` when a non-allowlisted member connects.
  (If a full two-process CLI e2e is too heavy for CI determinism, assert the owner-visibility at the
  net layer via the `StderrPipeAudit`/`PipeAuditSink` and keep the CLI test to argument/validation
  surface — see §7.)
- **`--help` snapshots** for `pipe close` show `<PIPE_ID>` only (no `<ROOM_ID>`), locking the surface.

### 5.8 WI-8 — docs reconciliation

- `docs/getting-started.md` Step 6: remove the `[reconcile]` note about `pipe close` taking both
  positionals; show `iroh-rooms pipe close <PIPE_ID>`; keep the security-warning and loopback
  language; keep the "closes on owner process exit" line and add the SIGKILL bound as a short caveat.
- `README.md`: update the #14 pipe bullet (or add a #23 paragraph) to state the reconciled
  `pipe close <PIPE_ID>` surface and the owner-visible reject logging.
- `crates/iroh-rooms-cli/tests/docs_conformance.rs`: update the pipe-close assertion
  (`guide_documents_pipe_close_command`, ~line 331) to expect `pipe close <PIPE_ID>` and add a check
  that the guide documents the loopback default and the owner-exit behaviour. Ensure the existing
  `pipe.connect.rejected` troubleshooting assertion still passes.

---

## 6. Data / API / event model impact

- **Event schema:** none. `pipe.opened` / `pipe.closed` (§7) are unchanged; `pipe.closed{closed}`
  and `pipe.closed{owner_exit}` are both already valid reasons.
- **Store:** one additive read-only method (`room_ids`), no schema change, no `user_version` bump.
- **CLI surface:** `pipe close` loses its `<ROOM_ID>` positional and gains an optional `--room`.
  `expose`/`connect` unchanged. A new `-v/--verbose` flag on `expose` (optional; default prints
  security-relevant reject/teardown lines).
- **Wire/network:** none. Same ALPN, same frames, same gate.
- **Audit vocabulary:** unchanged strings (`pipe.opened`, `pipe.closed`,
  `pipe.connect.accepted`, `pipe.connect.rejected:<cause>`, `pipe.torndown:<cause>`); #23 only adds a
  *stderr rendering* of the existing vocabulary.

---

## 7. Test strategy

| Test | Level | Locks |
|---|---|---|
| `room_ids()` unit tests | core/store | Room inference substrate (§5.1) |
| HTTP-server e2e: authorized GET round-trip | net e2e | §15.7 AC1/AC4/AC6; issue AC "explicit allowlist" |
| HTTP-server e2e: non-allowlisted + non-member denied, 0 bytes to server | net e2e | §15.7 AC5; issue AC "unauthorized rejected" |
| Owner audit shows `pipe.connect.rejected:<cause>` on reject | net / CLI | issue AC "rejected **and locally logged**" |
| HTTP-server e2e: clean close → `pipe.closed` on log → reconnect fails `closed` | net e2e | §15.7 AC7/AC8; issue AC "clean close emits `pipe.closed`" |
| `pipe close <PIPE_ID>` single-room inference publishes `pipe.closed` | CLI | issue scope `close <pipe-id>`; PRD §16 |
| `pipe close` ambiguous / absent-pipe errors | CLI | Robustness of §4.1 inference |
| expose: non-loopback `--tcp` refused; empty `--allow` refused | CLI | §13.2.2/§13.2.3; issue AC "explicit allowlist", "loopback default" |
| expose: non-member refused before IO | CLI | Least-privilege pre-check |
| Security warning names target + allowed member; stderr/stdout split | CLI | §13.2.4; issue Security Note |
| owner-exit publishes `pipe.closed{owner_exit}` on SIGINT and SIGTERM | net/CLI | issue AC "closes on owner process exit"; §13.2.5 |
| `--help` for `pipe close` shows `<PIPE_ID>` only | CLI | Surface lock vs PRD |
| docs-conformance: guide shows `pipe close <PIPE_ID>`, loopback default, reject code | CLI | Docs/PRD alignment |

**Determinism:** all tests run on the loopback/CI stack (the hidden `--loopback` flag / in-process
`Node`s), no relays, no wall-clock reads in decision paths, every await timeout-bounded — matching
`pipe_e2e.rs`, `message_e2e.rs`, and `join_e2e.rs`. The full gate is `scripts/verify.sh` (fmt
`--check`, clippy `-D warnings` pedantic, workspace tests) — the real CI gate; `cargo test` passing
alone is **not** sufficient.

**Two-process CLI e2e note.** A genuine two-**process** CLI test (spawn `pipe expose` and
`pipe connect` as separate binaries and scrape stderr) is the highest-fidelity way to assert
"locally logged," but is flaky under CI (port/discovery timing). Recommendation: assert the
owner-visible reject line at the **net layer** via the `PipeAuditSink` (deterministic, in-process),
and keep `pipe_cli.rs` focused on argument/validation/room-inference surface driven by `assert_cmd`.
If a two-process smoke test is wanted, gate it behind an `#[ignore]` + a `--loopback` opt-in so CI
stays green (mirror any existing pattern in `message_e2e`/`join_e2e`).

---

## 8. Security, privacy, reliability

- **Authorization is unchanged and already conformance-tested.** No-default-all, `allowed_members ∩
  Active`, owner-must-be-Active, `pipe.closed`-known, expiry-fail-closed, revocation-on-learn — all
  landed in #14 and covered by `pipe_e2e.rs` P1–P6 and the membership conformance suite. #23 must not
  alter any of it; it only adds tests and surfaces.
- **Loopback-only exposure** (§13.2.3): `--tcp` must be a loopback target (enforced by
  `is_loopback_target`); the connector binds `127.0.0.1` only. Locked by a CLI test.
- **Explicit warning** (§13.2.4 / issue Security Note): the warning names the target and allowed
  member(s); it is on stderr so it is visible even when stdout is redirected/parsed.
- **Owner-exit bound** (§13.2.5): graceful on SIGINT/SIGTERM. **Residual (documented, not solved):**
  SIGKILL / power loss leaves a `pipe.opened` with no `pipe.closed`; forwarding still stops (owner
  endpoint death → connector teardown), but `pipe list` shows it open until an owner/admin `close`.
  This is the §5 reachability bound, surfaced honestly (PRD §16.4 honesty rule), not a defect.
- **Local audit** (§13.2.7): reject/teardown/accept lines are now visible to the owner; secret
  material never appears in any output path (identity/device secrets stay in `Zeroizing`; the pipe
  `capability` model does not apply here — pipe access is snapshot-authorized, not secret-bearing).
- **Privacy:** `pipe.opened` carries `owner_endpoint`, `label`, `target_hint`, and `allowed_members`
  onto the shared log — as designed in §7. No new fields; no additional disclosure from #23.
- **Reliability:** both peers must be online (PRD §14.3); no queueing. `connect` fails clearly if the
  `pipe.opened` has not synced (existing 10s `SYNC_WAIT`).

---

## 9. Acceptance criteria (issue) → mechanism → test

| # | Issue acceptance criterion | Where satisfied | Locking test (this issue) |
|---|---|---|---|
| 1 | Pipe access requires explicit allowlist | Landed (`allowed_members` non-empty; `pipe_connect_allowed`) | HTTP e2e authorized/denied; CLI empty-`--allow` refusal |
| 2 | Local bind defaults to loopback | Landed (`is_loopback_target`; connector binds `127.0.0.1`) | CLI non-loopback `--tcp` refusal; e2e binds loopback |
| 3 | Unauthorized connect rejected **and locally logged** | Decision landed; **visibility added (§5.3)** | Owner audit asserts `pipe.connect.rejected:<cause>` |
| 4 | Clean close emits `pipe.closed` | Landed (`pipe close` → `pipe.closed{closed}`) | HTTP e2e close; `pipe close <PIPE_ID>` CLI test |
| 5 | Pipe closes on owner process exit | SIGINT landed; **SIGTERM added (§5.5)** | owner-exit test (SIGINT + SIGTERM); documented SIGKILL bound |

Plus the PRD §15.7 journey ACs (AC1 expose TCP, AC2 explicit authorize, AC3 `pipe.opened`, AC4
authorized connect, AC5 unauthorized rejected, AC6 encrypted transport, AC7 close, AC8
`pipe.closed`) — all mechanically landed; #23 locks AC4/AC5/AC7/AC8 additionally via the HTTP e2e and
the reconciled `close` surface.

**Definition of done:** the CLI presents `pipe close <PIPE_ID>` (no room id); an unauthorized connect
prints a reject line on the owner's terminal; `scripts/verify.sh` is green; the HTTP-server e2e and
`pipe_cli.rs` pass; docs and docs-conformance reflect the reconciled surface.

---

## 10. Risks & mitigations

| Risk | Sev | Mitigation |
|---|---|---|
| Room inference wrong/ambiguous in multi-room DBs | Med | Fail-closed on 0/≥2 matches with actionable message; `--room` override; unit-tested |
| Threading a `PipeAuditSink` into `Node::spawn` is more invasive than expected | Med | If `Node` hard-codes `TracingPipeAudit`, prefer the smallest additive parameter; if that balloons, fall back to installing a **scoped**, stderr-only `tracing` layer *inside `expose`* filtered to `pipe.*` targets (Decision §4.3 alt) |
| Two-process CLI e2e is flaky on CI | Med | Assert owner visibility at the net layer (deterministic); keep CLI tests to surface/validation; `#[ignore]` any real two-process smoke |
| Reviewer scope-creep into the gate/schema | Med | §3.2 non-goals are explicit; any change under `membership/access.rs`, `event/*pipe*`, or `pipe/gate.rs` is out of scope |
| Hidden coupling: docs-conformance test asserts the *old* `pipe close` form | Low | WI-8 updates the assertion in lockstep; run `docs_conformance.rs` as part of DoD |
| Hard-kill leaves stale `pipe.opened` on the log | Low | Documented bound (§8); forwarding stops regardless; owner/admin `close` reconciles |
| Gate-A real-NAT still owed | Low (pre-existing) | Out of scope; loopback-deterministic here; tracked in `NOTES.md` |

---

## 11. Assumptions

1. #14 (mechanism), #21 (offline read helpers/`display.rs`), and #12 (membership fold) are landed on
   the branch this work builds on — confirmed in the tree as of 2026-07-01.
2. `Node`'s pipe plane can accept a caller-supplied `PipeAuditSink` (the default is
   `TracingPipeAudit`); if not, threading one through is a small additive change (§10 mitigation).
3. MVP-sized rooms: folding each local room to resolve a `pipe_id` in `pipe close` is acceptable
   latency (a few rooms, bounded logs); a targeted SQL lookup is a later optimization, not required.
4. The hidden `--loopback` flag remains the CI/test network stack (used by every online CLI test).
5. No released consumers depend on `pipe close <ROOM_ID> <PIPE_ID>` (pre-1.0, Phase-0, unreleased).

---

## 12. Open questions

1. **Keep a back-compat `pipe close <ROOM_ID> <PIPE_ID>` form?** Recommendation: no (§4.5) — clean
   single-positional to match the PRD. Confirm before deleting the two-positional path.
2. **`-v/--verbose` default for accept lines.** Recommendation: print rejects/teardowns by default,
   accepts only under `-v`. Acceptable? (Rejects are the security-relevant events.)
3. **Audit sink vs scoped tracing layer.** Recommendation: a dedicated `StderrPipeAudit`
   (§4.3). If the team prefers a single logging path, a `pipe.*`-filtered stderr `tracing` layer
   scoped to the `expose` command is the fallback — which does the team want as the house pattern?
4. **`room_ids()` home vs a targeted `find_pipe_opened`.** Recommendation: ship `room_ids()` (generic,
   reusable by a future `room ls`); add the targeted query only if profiling demands. Agree?
5. **Two-process CLI smoke test.** Do we want an `#[ignore]`d real two-binary
   expose↔connect↔reject smoke test for manual/nightly runs, or is net-layer visibility assertion
   sufficient for CI?
