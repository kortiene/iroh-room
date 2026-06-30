# Spec: Getting-Started Demo Script

| | |
|---|---|
| **Issue** | #35 — [IR-0210] Write getting-started demo script |
| **Parent** | #3 |
| **Depends on** | #34 |
| **Labels** | type/docs, area/cli, area/dx, priority/p1, risk/low |
| **Traceability** | `PRD.v0.3.md` §6 (MVP demo), §14 (Availability), §15 (User journeys), §16 (CLI), §17.2 (DX metrics), §19 Phase 1B; `PHASE-0-SPIKE.md` §8 (Rejection taxonomy), §5–§6 (Pipe/blob/invite auth) |
| **Status** | Draft spec — ready for an author to execute |
| **Type** | Documentation deliverable (no production code changes) |

---

## 1. Summary

Produce a repeatable, copy-pasteable **getting-started demo** that walks a developer,
from a clean checkout and a fresh local data directory, through the full MVP flow:

> create identity → create room → invite & join a second human → exchange signed
> messages → share & verify a file → expose & connect a live TCP pipe → post & read
> an agent status.

The deliverable is **documentation** (a guide, the "demo script" in runbook sense), not
production code. It must be honest about the availability model (no guaranteed offline
delivery) and must document the four required failure modes with concrete next actions.

This spec defines **what to write, where, in what structure, with which exact commands and
expected outputs, and how to validate it**. It is detailed enough for another engineer or
agent to execute without re-deriving scope.

---

## 2. Background & current repository state

Read before authoring:

- `PRD.v0.3.md` — §6 (the canonical MVP demo, 10 steps), §14 (availability model — the
  honesty contract), §15 (per-journey acceptance criteria + example commands), §16 (full
  CLI command surface + CLI UX requirements, esp. §16.2 pipe security warning and §16.3
  "distinguish offline peer, unauthorized peer, unavailable blob, invalid ticket, invalid
  signature"), §17.2 (DX metrics this demo is measured against).
- `PHASE-0-SPIKE.md` — §8 (Rejection / Flag taxonomy: the stable reason codes the CLI
  surfaces), §5 (pipe/blob authorization at connect time), §6 (invite tickets as
  capabilities; key-bound, expiry, no native revocation), §1/§7 (identity vs device key,
  event-type registry).
- `README.md`, `CONTRIBUTING.md`, `scripts/verify.sh`.

**Critical current-state facts the author must account for:**

1. **The CLI is not yet implemented.** `crates/iroh-rooms-cli/src/main.rs` is a scaffold
   that prints `iroh-rooms CLI scaffold v1`. The full command surface in PRD §16 is the
   **intended** surface, not yet the shipped one.
2. **This issue depends on #34.** The demo guide can only be finalized and validated once
   the CLI commands it exercises exist (delivered under #34 and the other Phase 1B issues:
   identity, room, invite/join, message, file share/fetch, pipe, agent status). See §11
   (Dependencies & sequencing).
3. **The binary, not this spec or the PRD, is the source of truth for exact syntax and
   output.** Where the shipped CLI diverges from PRD §16, the shipped CLI wins for the
   guide; any divergence should be reported back so the PRD/CLI can be reconciled.

Because of (1)–(3), every command and every "Expected output" block in the guide MUST be
**captured from a real run of the merged binary**, never invented. Placeholder *shapes*
(IDs, tickets) may be illustrative; surrounding prose, flags, and output framing must match
reality at authoring time.

---

## 3. Scope

### In scope

- A new guide at **`docs/getting-started.md`** containing the full demo walkthrough.
- A short link/pointer added to **`README.md`** ("Getting Started" section).
- The availability-model explanation (inline section in the guide).
- A troubleshooting section covering **offline peer, unauthorized peer, invalid ticket,
  unavailable file** (the four required by the issue) plus the closely related
  invalid-signature / non-member cases the CLI already distinguishes (PRD §16.3).
- A "reset to a clean state" section so the guide is repeatable.

### Out of scope

- Implementing or changing any CLI command (that is #34 and sibling Phase 1B issues).
- Multi-machine / real-NAT walkthrough as the *primary* path (mention as an optional
  variant only; the canonical demo runs on a single host — see §5.2).
- Calls, mobile/desktop UX, large rooms, public discovery (all out of MVP per PRD §7.3).
- Marketing copy, screenshots/asciinema (a follow-up may add recordings; not required here).
- Any claim of guaranteed offline delivery, cloud inbox, or always-on availability.

---

## 4. Deliverables (files)

| File | Action | Notes |
|---|---|---|
| `docs/getting-started.md` | **create** | Primary deliverable. Structure in §6. |
| `README.md` | **edit** | Add a "Getting Started" pointer linking to the guide. Keep it to a couple of lines; do not duplicate the walkthrough. |
| `docs/` | **create dir** | If it does not exist (the ADW documentation gate already whitelists `docs/`). |

No production code, no `Cargo.*`, no `scripts/` changes. `scripts/verify.sh` is unaffected
(it runs fmt/clippy/test; Markdown is not gated by it).

---

## 5. Demo environment model (decide and document explicitly)

The demo requires **at least two human peers + one agent**, all of which the developer must
be able to run themselves. Two decisions must be made and stated plainly in the guide.

### 5.1 Multiple identities on one host → isolated data directories

Each participant (Alice, Bob, Agent) is a **separate identity with its own local store**.
On a single machine they must not share state, so each runs the CLI against a **distinct
data directory**, in its own terminal.

- The guide MUST define how to point the CLI at a per-participant data directory. Use
  whatever the shipped binary supports — expected to be an env var (e.g.
  `IROH_ROOMS_HOME` / `IROH_ROOMS_DATA_DIR`) and/or a `--data-dir` global flag.
- **If the merged CLI has no data-dir override**, the single-host three-identity demo is
  impossible and this is a hard blocker: file it against the CLI (see Open Questions Q1)
  before writing the walkthrough. Do not ship a guide that silently assumes one.
- Establish three shells up front, e.g.:

  ```bash
  # Terminal A (Alice)
  export IROH_ROOMS_HOME="$PWD/.demo/alice"
  # Terminal B (Bob)
  export IROH_ROOMS_HOME="$PWD/.demo/bob"
  # Terminal C (Agent)
  export IROH_ROOMS_HOME="$PWD/.demo/agent"
  ```

  Mark `IROH_ROOMS_HOME` and the `.demo/*` paths as placeholders/conventions, and reconcile
  the variable name with the actual binary.

### 5.2 Connectivity: single host first, two machines as a variant

- **Canonical path:** all three participants on **one machine**. Peers discover/connect
  over local discovery (mDNS) and/or relay fallback; this is the lowest-friction way to hit
  the DX timing targets (PRD §17.2). This is what the Test Plan (§9) validates.
- **Optional variant (appendix, not required to pass):** two machines on real networks.
  Cross-reference `PHASE-0-SPIKE.md` Gate A (NAT/relay) and note that direct hole-punching
  is environment-dependent and relay fallback may be used. Do not block the main guide on it.

### 5.3 Placeholder convention (used throughout)

Document a single legend near the top and use it consistently:

| Placeholder | Meaning | Produced by |
|---|---|---|
| `<ROOM_ID>` | Room identifier | `room create` output |
| `<BOB_TICKET>` | Invite ticket string for Bob | `room invite` output |
| `<AGENT_TICKET>` | Invite ticket / handle for the agent | `agent invite` (or `room invite --role agent`) output |
| `<BOB_ID>` / `<AGENT_ID>` | Member identity key (hex) | `identity show` / `room members` output |
| `<FILE_ID>` | File handle | `file share` / `file list` output |
| `<PIPE_ID>` | Pipe session id | `pipe expose` / `pipe list` output |

Rules: placeholders are `<UPPER_SNAKE_IN_ANGLE_BRACKETS>`; every command line that contains
one is preceded by a one-line note on where to copy the value from; never embed a real
machine-specific value as if it were copy-pasteable.

---

## 6. Structure of `docs/getting-started.md`

Author the guide in this order. Each numbered step in §6.4–§6.10 MUST contain three
labelled parts: **Command** (copy-pasteable, placeholders marked), **Expected output**
(captured from a real run; may elide volatile bytes with `…`), and **What this proves /
verify** (one or two lines tying back to a PRD acceptance criterion).

### 6.1 Title + one-paragraph framing

What the reader will accomplish and roughly how long it takes (tie to PRD §17.2: first
identity < 1 min; first two-peer room < 3 min; first pipe < 5 min). State plainly: this is a
local-first, peer-to-peer runtime with **no central server and no guaranteed offline
delivery** (forward-reference the Availability section).

### 6.2 Prerequisites

- Supported OS (macOS/Linux per current dev target), Rust toolchain version
  (workspace `rust-version = "1.80"`), `git`.
- Clean checkout + build:

  ```bash
  git clone https://github.com/kortiene/iroh-room.git
  cd iroh-room
  cargo build --release
  ```

- How the binary is invoked in the guide. Pick ONE convention and use it everywhere:
  either `cargo run --release -p iroh-rooms-cli -- <args>` or a built binary path aliased to
  `iroh-rooms` (preferred for readability):

  ```bash
  alias iroh-rooms="$PWD/target/release/iroh-rooms"
  ```

  **Verified (2026-06-29):** the Cargo package is `iroh-rooms-cli` but the produced binary is
  named `iroh-rooms` (`crates/iroh-rooms-cli/Cargo.toml` → `[[bin]] name = "iroh-rooms"`), so
  the built artifact is `target/release/iroh-rooms` and the package selector for `cargo run`
  is `-p iroh-rooms-cli`. This matches the `iroh-rooms …` invocation used in all PRD §16
  examples — use it verbatim. Re-confirm against the build only if the bin stanza changes.

### 6.3 Set up the three participants

Apply §5.1 (three terminals, three `IROH_ROOMS_HOME` values). Show the commands to create
the `.demo/*` directories. Note these dirs are the "fresh local data directory" the Test
Plan resets between runs.

### 6.4 Step 1 — Create identities (PRD §15.1)

- **Alice** (Terminal A): `iroh-rooms identity create --name "Alice"` then
  `iroh-rooms identity show`.
- **Bob** (Terminal B): same with `--name "Bob"`.
- **Agent** (Terminal C): same with `--name "build-agent"` (this is the agent's own
  identity; PRD §13.3 / spike §1 — agents are ordinary principals with their own key).
- Verify: each prints an identity key; no central account required; identity persists in the
  participant's `IROH_ROOMS_HOME`. Capture `<BOB_ID>` and `<AGENT_ID>` from `identity show`.

### 6.5 Step 2 — Alice creates the room (PRD §15.2)

- `iroh-rooms room create "Getting Started Room"` → capture `<ROOM_ID>`.
- Verify: Alice is admin (single immutable admin, spike §3.1); `room.created` is signed and
  stored locally; show `iroh-rooms room members <ROOM_ID>` listing Alice as admin.

### 6.6 Step 3 — Invite and join (PRD §15.3, spike §6)

- Alice: `iroh-rooms room invite <ROOM_ID> --expires 24h` → capture `<BOB_TICKET>`.
  - Note in the guide: tickets are **scoped, key-bound, single-room capabilities** with a
    secret carried out-of-band in the ticket; expiry is supported, native revocation is not
    (spike §6 / §6 "MVP limitations"). Treat a ticket like a password.
- Bob: `iroh-rooms room join <BOB_TICKET>`.
- Agent: invite + join the agent. Show whichever the binary supports:
  `iroh-rooms agent invite <ROOM_ID> <AGENT_ID>` (PRD §16) and the agent joining via its
  ticket/handle. Reconcile exact agent-invite syntax with the binary.
- Verify: `iroh-rooms room members <ROOM_ID>` (run by Alice **and** Bob) now lists Alice,
  Bob, and the agent; the agent shows `role = agent`.

### 6.7 Step 4 — Send and read messages (PRD §15.4)

- Bob: `iroh-rooms room send <ROOM_ID> "I pushed the first prototype."`
- Alice: `iroh-rooms room tail <ROOM_ID>` (streaming) shows Bob's message.
- Optionally Alice replies and Bob tails.
- Verify: messages are signed, delivered in < 2 s when both online (PRD §17.1.3), stored
  locally, duplicates ignored, invalid signatures / non-members rejected (forward-ref the
  Troubleshooting reason codes). **Keep both peers online for this step** — do not stage a
  send-while-offline-then-receive flow (that would imply guaranteed offline delivery).

### 6.8 Step 5 — Share and verify a file (PRD §15.6, §9.2; spike §5 blob gate)

- Create a small sample file (the guide provides the `echo`/`printf` command so the demo is
  self-contained, e.g. a tiny `hello.txt`).
- Alice: `iroh-rooms file share <ROOM_ID> ./hello.txt` → capture `<FILE_ID>`.
- Bob: `iroh-rooms file list <ROOM_ID>` then `iroh-rooms file fetch <ROOM_ID> <FILE_ID>`.
- Verify: the fetched bytes are content-verified against the declared hash (BLAKE3); the CLI
  confirms integrity. **Explicitly state the availability caveat here**: the file is
  fetchable **only while a peer that holds it (Alice) is online**; if no provider is online
  the CLI reports an unavailable state honestly (PRD §9.2, §15.6 #6). This sets up the
  "unavailable file" troubleshooting case.

### 6.9 Step 6 — Expose and connect a live pipe (PRD §15.7, §16.2; spike §5)

- Start a trivial local TCP service the reader can run with no extra install (e.g.
  `python3 -m http.server 3000` on Alice's machine). Provide the exact command.
- Alice: `iroh-rooms pipe expose <ROOM_ID> --tcp localhost:3000 --allow <BOB_ID>`
  → capture `<PIPE_ID>`.
  - **Reproduce the CLI security warning verbatim** (PRD §16.2 / §13.2): exposing a local
    service, only `<BOB_ID>` is authorized, loopback bind, the pipe id, and the close
    command. The guide must show this warning, not paraphrase it away.
- Bob: `iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local 3001`, then in another shell
  `curl http://localhost:3001` to prove traffic flows over the authenticated P2P pipe.
- Alice closes: `iroh-rooms pipe close <PIPE_ID>` (emits `pipe.closed`); also note pipes
  close on owner process exit.
- Verify: authorized peer connects; **unauthorized peer is rejected** (forward-ref
  Troubleshooting); traffic is encrypted peer-to-peer; both peers must be online (PRD §14.3).

### 6.10 Step 7 — Agent status (PRD §15.8)

- Agent (Terminal C): `iroh-rooms agent status <ROOM_ID> "Running integration tests…"`.
- Alice/Bob: see it in `room tail` / a status view.
- Verify: agent events are signed by the agent's own key; the agent could not have posted
  without an explicit invite (spike §3.5 authorization gate); agent is a first-class but
  not-implicitly-trusted participant (PRD §13.3).

### 6.11 (Recommended, optional) Persistence & reconnect

Short addendum showing local history survives a restart (PRD §6 step 6 / §17.1.7): stop and
relaunch a participant; `room tail` still shows prior events from the local store. Frame
reconnect/sync honestly (recent-window sync between online peers; not a guaranteed inbox).
Mark as optional so it does not bloat the timed core flow.

### 6.12 Availability model (REQUIRED — its own section)

Restate PRD §14 in plain developer language. Must include, as explicit bullets:

1. Messages deliver when peers are online or reconnect through available peers.
2. Files are fetchable **only** when at least one peer holding the file is online.
3. Live pipes require **both** peers online.
4. There is **no cloud inbox** and **no guaranteed offline delivery**.
5. No central application server by default; optional infrastructure (always-on node,
   archive peer, relay) can improve reliability later but never owns your room.

This section is what PRD §17.2.5 measures ("≥80% of test users can correctly explain the
availability model"). Write it so a reader can restate it in one breath. **Do not** include
any earlier demo step that contradicts these bullets.

### 6.13 Troubleshooting (REQUIRED — four named failure modes + next actions)

For each case: **how to reproduce it in this demo**, **what the CLI prints** (the stable
reason code from spike §8 and the human message), and **the next action**. The four the
issue requires, mapped to reason codes:

| Failure mode | Reproduce | Reason code(s) (spike §8 / §5) | Next action |
|---|---|---|---|
| **Offline peer** | Stop Alice, have Bob `file fetch` / try `pipe connect` / send a message | connection-state "no provider online" / peer-unreachable (PRD §16.3 "offline peer") | Bring the peer back online; nothing is queued on a server — retry when both are online. |
| **Unauthorized peer** | A member NOT in `--allow` tries `pipe connect`; or a non-member tries to act | `pipe.connect.rejected`; mesh `not_a_member`; blob `AbortReason::Permission` | Have the pipe owner re-`expose` with the correct `--allow <ID>`, or have the admin invite the peer. |
| **Invalid ticket** | Bob joins with a garbled or expired ticket | `bad_capability` (malformed/secret mismatch) / `expired_invite` | Ask the admin for a fresh `room invite` (re-issue, optionally longer `--expires`). |
| **Unavailable file** | Alice (the only provider) offline, Bob `file fetch` | unavailable / no-provider-online (PRD §9.2, §15.6 #6) | Wait until a peer holding the file is online; MVP has no pinning/always-on node yet. |

Also briefly cover the adjacent cases the CLI distinguishes (PRD §16.3): **invalid
signature** (`bad_signature`) and **non-member event** (`not_a_member`) — one line each,
since they share the "rejected & logged" model. Point readers to the local audit log for
pipe open/connect/reject/close events (spike §5, PRD §13.2).

Every troubleshooting entry MUST end with a concrete next action (issue acceptance
criterion: "Failure modes are documented with next actions").

### 6.14 Reset / clean up

Show how to return to a clean state so the demo is repeatable and matches the Test Plan:
close pipes, stop processes, and remove the per-participant data dirs
(`rm -rf .demo/alice .demo/bob .demo/agent`). Warn that this deletes local identities and
room history (local-first; no server copy).

### 6.15 Next steps / references

Link to `PRD.v0.3.md`, `PHASE-0-SPIKE.md`, `CONTRIBUTING.md`, and the issue backlog.

---

## 7. Authoring guidelines (voice, honesty, copy-paste correctness)

1. **Copy-pasteable:** every command runs as written once placeholders are substituted. No
   prose interleaved inside a code block. One logical action per block. Long commands stay
   on one line or use explicit `\` continuations.
2. **Placeholders clearly marked** per §5.3; never present a host-specific literal as
   reusable.
3. **Real outputs only:** capture "Expected output" from actual runs; elide volatile data
   (hashes, ports, timestamps) with `…` but keep structure faithful. Do not fabricate.
4. **Honesty over polish:** never imply guaranteed delivery, a server inbox, or always-on
   availability. Prefer "when both peers are online" phrasing.
5. **Security framing:** keep the pipe security warning prominent; treat tickets as secrets.
6. **Scriptable tone:** terse, imperative, developer-to-developer. Annotate each step with
   the one PRD acceptance criterion it demonstrates.
7. **Consistency:** one binary-invocation convention, one data-dir convention, one
   placeholder style throughout.
8. Run the `writing-guidelines` review pass if available before finalizing.

---

## 8. Implementation steps (for the author/agent executing this spec)

1. Confirm **#34 is merged** and the CLI subcommands the demo needs exist. If any are
   missing, stop and record the gap (do not write a guide against vapor). See §11.
2. Build the binary; run `iroh-rooms --help` and each subcommand's `--help`. Record the
   **actual** command syntax, flags, and the data-dir override mechanism (§5.1).
3. Create `docs/` and draft `docs/getting-started.md` following the §6 structure.
4. **Execute the entire demo end-to-end yourself** on one host with three isolated data
   dirs, three terminals. Paste real outputs into the "Expected output" blocks.
5. **Deliberately trigger each of the four failure modes** (§6.13) and capture the exact
   reason codes / messages the CLI emits. Reconcile any mismatch with spike §8; if the CLI
   emits a code not in §8, report it.
6. Time the flow against PRD §17.2 (identity < 1 min, two-peer room < 3 min, pipe < 5 min).
   If a target is missed for documentation reasons (unclear step), tighten the prose.
7. Add the README "Getting Started" pointer.
8. Reset to a clean checkout + fresh data dirs and **run the guide verbatim** (Test Plan §9)
   — ideally have someone who did not write it run it (proxy for PRD §17.2.4).
9. Run `scripts/verify.sh` (should be unaffected by docs, but confirm nothing else changed).
10. Open the PR linking #35; note in the description any PRD §16 vs shipped-CLI divergences
    found.

---

## 9. Test plan / validation

From the issue: **"Run the guide from a clean checkout and fresh local data directory."**

- **Clean-room run:** fresh `git clone` (or `git clean -xdf` + new `.demo/*` dirs); follow
  the guide top to bottom with zero outside knowledge; every command must succeed and every
  "Expected output" must match (modulo elided volatile bytes).
- **Failure-mode run:** reproduce all four troubleshooting cases; confirm the documented
  reason code and next action are accurate.
- **Availability comprehension:** a reviewer who followed the guide can restate the five
  availability bullets (§6.12) unaided (proxy for PRD §17.2.5).
- **Timing:** informally confirm the PRD §17.2 targets are reachable by a first-timer.
- **No-maintainer-help:** a developer not involved in authoring completes the full flow from
  docs alone (PRD §17.2.4). Strongly recommended before merge.
- **Gate:** `scripts/verify.sh` still passes (docs-only change; sanity check only).

---

## 10. Acceptance criteria (from the issue, made verifiable)

- [ ] **Full flow from docs:** a developer can complete identity → room → invite/join →
  message → file → pipe → agent-status entirely from `docs/getting-started.md`. *(Verify via
  the clean-room + no-maintainer-help runs in §9.)*
- [ ] **Copy-pasteable with marked placeholders:** every command runs as written after
  substituting `<…>` placeholders; a placeholder legend exists (§5.3). *(Verify by running
  verbatim.)*
- [ ] **Failure modes documented with next actions:** offline peer, unauthorized peer,
  invalid ticket, unavailable file each appear with reproduction, reason code, and a
  concrete next action (§6.13). *(Verify via the failure-mode run.)*
- [ ] **No implied guaranteed offline delivery:** the availability section states the five
  PRD §14 bullets and no step contradicts them. *(Verify by review of §6.12 and every prior
  step.)*
- [ ] Guide lives at `docs/getting-started.md`; README links to it; no production code
  changed; `scripts/verify.sh` passes.

---

## 11. Dependencies & sequencing

- **#34 (dependency):** the demo cannot be validated until the CLI commands it exercises
  exist and behave as documented. #34 plus the other Phase 1B issues (PRD §19: recent sync,
  blob import, file shared/fetch, agent identity/status, pipe security warnings, human+agent
  integration test) collectively provide that surface. This guide is the **last** Phase 1B
  deliverable ("Demo script and getting started docs").
- If #35 is scheduled before the CLI is feature-complete, the draft can be written against
  PRD §16 as a skeleton, but **acceptance cannot be claimed** until it has been executed
  end-to-end against the real binary (§8, §9).

---

## 12. Risks & mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| CLI not implemented / diverges from PRD §16 when writing | Guide describes commands that don't exist or differ | Gate authoring on #34; capture all syntax/output from the real binary; report divergences (§2, §8). |
| No single-host data-dir override in the CLI | Three-identity demo impossible on one machine | Verify §5.1 early; file a CLI blocker if missing (Open Q1). |
| Implying guaranteed offline delivery | Misleads users; violates an explicit acceptance criterion + PRD §14 | Dedicated availability section; keep both peers online in every send/fetch/pipe step; honesty review pass. |
| Reason codes drift from spike §8 | Troubleshooting becomes wrong | Capture actual emitted codes; cross-check §8; flag mismatches. |
| Output drift over time (hashes, version strings) | Guide rots | Elide volatile bytes with `…`; keep outputs structural, not exact, where they change per run. |
| P2P connectivity flakiness on author's network | Steps appear to fail | Use single-host + loopback as canonical; document relay fallback expectation; keep two-machine path as an optional appendix. |
| Pipe step exposes a real local service | Security footgun if copied carelessly | Reproduce the CLI security warning; use a throwaway `http.server`; show explicit `--allow` and `pipe close`. |

---

## 13. Assumptions

1. The shipped CLI roughly matches PRD §16 (`identity`, `room`, `file`, `pipe`, `agent`
   command groups). Exact flags/output are reconciled against the binary at authoring time.
2. The CLI supports per-invocation data-dir isolation (env var and/or `--data-dir`),
   enabling multiple identities on one host (§5.1).
3. The CLI distinguishes the failure modes in PRD §16.3 and surfaces stable reason codes
   aligned with spike §8.
4. The canonical demo runs on a single machine; multi-machine is an optional appendix.
5. macOS/Linux dev environment with `python3` and `curl` available for the pipe step
   (the guide may offer a `nc`-based alternative if `python3` is unavailable).
6. `docs/` is an acceptable home for developer docs (already whitelisted by the ADW
   documentation gate in `.adw/config.json`).

---

## 14. Open questions

1. **Data-dir override:** what is the exact mechanism and name (`IROH_ROOMS_HOME`,
   `IROH_ROOMS_DATA_DIR`, `--data-dir`)? Needed for §5.1. (Relates to PRD §21 Open Q8 — local
   DB path / backup-export story.)
2. **Agent invite/join syntax:** is it `agent invite <ROOM_ID> <AGENT_ID>` + a ticket join,
   or `room invite --role agent`? Confirm against the binary (PRD §16 shows `agent invite`).
3. **Binary name & invocation:** *Resolved (2026-06-29).* Cargo package = `iroh-rooms-cli`,
   produced binary = `iroh-rooms` (`crates/iroh-rooms-cli/Cargo.toml` `[[bin]] name`). Use
   `target/release/iroh-rooms` (or `cargo run -p iroh-rooms-cli --`) and the `iroh-rooms …`
   front-end name throughout, matching PRD §16. No action remaining unless the bin stanza
   changes.
4. **Exact security-warning text** for `pipe expose` — must be reproduced verbatim (PRD
   §16.2); pull from the implementation.
5. **Recordings:** should a follow-up add asciinema/screenshots? Out of scope here; flag if
   the team wants it as a sibling issue.
6. **Two-machine appendix depth:** how much NAT/relay guidance to include without turning the
   getting-started guide into a networking troubleshooting doc (cross-link spike Gate A
   instead).
