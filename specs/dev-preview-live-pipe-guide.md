# Spec: Document the dev-preview Live Pipe workflow (IR-0305)

| | |
|---|---|
| **Work item** | IR-0305 / GitHub issue #40 |
| **Parent** | #4 |
| **Type** | `type/docs` · `area/pipe` · `area/dx` |
| **Priority / risk** | p1 / low |
| **Traceability** | `PRD.v0.3.md` JTBD 2, §9.3 (Live Pipe Plane), §13.2 (Pipe Security), §16.2 (CLI security warning), §17.3 (product success), §18 (risks). PRD.v0.3 §14 availability. |
| **Depends on** | #23, #34, #35 (see [Assumptions](#assumptions) for the mapping used) |
| **Deliverable** | One new Markdown guide under `docs/` + index/cross-link edits. **No production code changes.** |

---

## 1. Goal

Ship a task-focused developer guide that proves Live Pipe's differentiated value:
"share a running local preview with one authorized peer **without** a public tunnel, cloud
deploy, or VPN" (PRD JTBD 2). The guide must walk the operator through the full
**expose → connect → close** flow, cover an **agent-generated** preview, document
**unauthorized-access** behavior, compare the workflow against public tunnels in **neutral
product language**, and state the **availability + relay-fallback** limits honestly.

This is a documentation-only work item. The pipe CLI (`iroh-rooms pipe expose|connect|close|list`)
and its net/gate implementation already exist and are conformance-tested; this spec adds a
guide on top of the landed surface. It does **not** add, change, or gate any behavior.

---

## 2. Current state (what exists vs. what is missing)

### Already landed (reuse, do not re-implement or re-document from scratch)

- **CLI surface** — `crates/iroh-rooms-cli/src/pipe.rs`, wired in `crates/iroh-rooms-cli/src/cli.rs`
  (`PipeAction::{Expose, Connect, Close, List}`). Behavior confirmed while writing this spec:
  - `pipe expose <ROOM_ID> --tcp <IP:PORT> --allow <IDENTITY_ID>...` — refuses non-loopback
    targets (PRD §13.2.3), requires ≥1 `--allow` (no default-all, PRD §13.2.2), requires the
    caller to be an **active** room member, prints the ⚠ SECURITY warning to **stderr** and the
    exposure summary + next-step hints to **stdout**, then serves until Ctrl-C / SIGTERM and
    publishes `pipe.closed{owner_exit}` on the way out.
  - `pipe connect <ROOM_ID> <PIPE_ID> --local <PORT>` — active-member-gated; waits ≤10 s for the
    `pipe.opened` to sync; forwards a loopback port; prints per-connection status lines; an
    unreachable owner exits `6` (`peer_offline`).
  - `pipe close <PIPE_ID> [--room <ROOM_ID>]` — owner or admin only; infers the room from the
    local log.
  - `pipe list <ROOM_ID>` — offline read of currently-open pipes.
- **Gate / audit vocabulary** — `crates/iroh-rooms-net/src/pipe/*`. Owner-side rejects/teardowns
  are surfaced on the operator's terminal as `pipe.connect.rejected:<cause>` /
  `pipe.torndown:<cause>` (causes: `not_allowed`, `not_active`, `closed`, `expired`,
  `unknown_device`, `owner_inactive`). `-v/--verbose` also logs accepts.
- **Existing demo coverage** — `docs/getting-started.md` **Step 6** already demonstrates the
  expose/connect/close flow end-to-end as one step of the canonical three-participant demo, and
  its **Availability model** + **Troubleshooting → Unauthorized peer / Offline peer** sections
  already carry the honesty language and exit codes.
- **Backing e2e test** — the authorized/unauthorized live-pipe pair is exercised by an
  `#[ignore]`-gated integration test (referenced from `docs/getting-started.md` "Status of this
  guide"). This spec's verification reuses it (see §7).

### Missing (this is the work)

1. A **standalone, goal-oriented guide** the reader can follow without wading through the full
   demo — the demo interleaves identity/room/message/file steps; the preview job needs its own
   entry point.
2. The **agent-generated preview** scenario (PRD §9.3 use-case 2). The demo's Step 6 is
   human-only.
3. The **public-tunnel comparison in neutral product language** — this exists **nowhere** in the
   repo today and is the core "differentiated value" ask (PRD §17.3.5, §2, §5.7).
4. A single place that states the **availability + relay-fallback** contract *for pipes
   specifically* (the demo's Availability model covers all planes at once).

---

## 3. Key decision — new standalone doc, cross-linked (not an edit to Step 6)

Create a **new** file `docs/live-pipe-preview.md` rather than expanding `getting-started.md`
Step 6. Rationale:

- The issue asks for a **guide that proves differentiated value** with a tunnel comparison and
  an agent scenario — that is a product-narrative doc, not another mechanical demo step.
- `getting-started.md` is already ~1100 lines and deliberately linear (one runnable happy path);
  bolting a comparison table + a second scenario into Step 6 would bloat it and blur its "one
  demo, in order" contract.
- A standalone doc gives IR-0305 a stable anchor to link from the README and the demo's "Next
  steps," and lets the demo's Step 6 stay the minimal in-line version.

**Single source of truth rule:** the new guide must **not** re-derive setup (identity / room /
invite). It links to `docs/getting-started.md` Steps 1–3 for "have a room with two active
members" and then owns the preview flow. Any fact also stated in Step 6 (exit codes, reject
causes, availability bullets) must be phrased **consistently** with Step 6 and the PRD — if a
number would differ, the doc is wrong. Prefer linking over duplicating.

---

## 4. Deliverable — `docs/live-pipe-preview.md`

Write the guide with the sections below **in this order**. Content requirements are
prescriptive; exact prose is the author's, but every listed fact/command/output must appear and
must match the landed CLI. Use the same voice/format conventions as `docs/getting-started.md`
(placeholder legend `<ROOM_ID>` / `<PIPE_ID>` / `<BOB_ID>`, "**Command**" / "**Expected
output**" / "**What this proves**" blocks, fenced `bash` / `text` blocks).

### 4.1 Title + one-paragraph value frame

- H1: `# Sharing a local preview over a Live Pipe`.
- Opening paragraph states the job (PRD JTBD 2, verbatim intent): you have a local dev server or
  an agent-generated preview and want **one** authorized peer to review it — **without** a
  public tunnel, cloud deploy, or VPN. Name the guarantee up front: the connection is
  peer-to-peer and authenticated, the target stays on loopback, and authorization is explicit
  and revocable.
- A "read first" callout mirroring `getting-started.md`'s status callout: this guide assumes you
  already have a room with two active members — link to `getting-started.md` Steps 1–3 — and
  that the `iroh-rooms` binary is on `PATH`.

### 4.2 What Live Pipe is (and is not)

Short bulleted framing, sourced from PRD §9.3 + §13.2 + §7.3:

- **Is:** authenticated TCP forwarding between exactly two authorized peers, loopback target,
  explicit per-member allow-list, room-scoped, session-lived, audited locally.
- **Is not (MVP):** terminal sharing, Unix-socket forwarding, multiplexed services, browser-native
  UX, public URLs, or any offline/queued delivery. State these as scope, not as failure.

### 4.3 Prerequisites (link, don't duplicate)

- A room where the previewer (call them **Reviewer / Bob**) and the presenter (**Presenter /
  Alice**, or an **Agent**) are both **active members** — link Steps 1–3 of `getting-started.md`.
- `python3` (or the `nc` fallback already documented in Step 6) to stand up a throwaway server;
  `curl` to prove traffic flows. Match Step 6's prerequisite wording.
- Note the LAN/CI hint: pass `--peer <PRESENTER_LISTENING_ADDR>` when discovery is unavailable
  (mirror the demo's `--peer` guidance).

### 4.4 Scenario A — a local web server preview (the core expose → connect → close flow)

This section satisfies **AC "expose/connect/close flow clearly"** and **AC "authorized peer can
view a local preview."** Structure it as four labeled steps with exact expected output.

**A1. Start a throwaway local server** (Presenter). Reuse Step 6's snippet exactly:

```bash
python3 -m http.server 3000
```

Include the same `nc` fallback note as Step 6 (Python-less environments; macos `nc -l -p 3000`).

**A2. Expose it to one reviewer** (Presenter, new shell):

```bash
# Substitute <ROOM_ID> and <BOB_ID> (the reviewer's identity key from `identity show`).
# --tcp requires an IP address; use 127.0.0.1, not the hostname "localhost".
iroh-rooms pipe expose <ROOM_ID> --tcp 127.0.0.1:3000 --allow <BOB_ID> --label web-preview
```

Show the exact expected output, calling out the stderr/stdout split (copy the shape from
`pipe.rs::expose`):

```text
⚠  SECURITY: exposing 127.0.0.1:3000 to 1 allowed member(s): 9f124ac1.
   Anyone allowed can reach 127.0.0.1:3000 through this pipe while it is open.
room: blake3:…(64 hex chars)…
target: 127.0.0.1:3000
label: web-preview
allow: 9f12…4ac1
listening: <ENDPOINT_ID>@<ip:port>
tip: share this address with connectors via --peer
pipe_id: 8hd3b29e1f4a7c0d2e5b6f8a9c1d3e4f
connectors run: iroh-rooms pipe connect blake3:… 8hd3b29e1f4a7c0d2e5b6f8a9c1d3e4f --local <PORT>
close it with: iroh-rooms pipe close 8hd3b29e1f4a7c0d2e5b6f8a9c1d3e4f
serving the pipe; press Ctrl-C to close it...
```

Explain: the ⚠ lines go to **stderr** so they survive stdout redirection; copy the `pipe_id:`
value (32 lowercase hex) as `<PIPE_ID>`. Mention the useful expose flags briefly and point to
the appendix (§4.9): `--allow` (repeatable, required), `--label`, `--expires <int>{s|m|h|d}`,
`--peer`, `-v/--verbose`.

**A3. Connect and view the preview** (Reviewer):

```bash
# Substitute <ROOM_ID> and <PIPE_ID> from the presenter's expose output (or `pipe list`).
iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local 3001
```

Expected connect output (from `pipe.rs::connect`):

```text
room: blake3:…
forwarding: 127.0.0.1:3001 -> pipe <PIPE_ID>
connect your client to 127.0.0.1:3001; press Ctrl-C to stop.
```

Then, in another Reviewer shell, prove traffic flows over the authenticated P2P pipe:

```bash
curl http://localhost:3001
```

Show that `curl` returns whatever the presenter's server serves (a directory listing), and note
the live status line the connector prints per connection (`[pipe] connection forwarding`). This
is the concrete proof for **AC "authorized peer can view a local preview."**

**A4. Close the pipe** (Presenter):

```bash
# No room id needed: the room is inferred from the local log. Add --room only to disambiguate.
iroh-rooms pipe close <PIPE_ID>
```

Expected: `closed pipe <PIPE_ID> in room blake3:…`. Then stop the `http.server` with Ctrl-C.
State the two close paths and their difference (this is also §4.6):

- explicit `pipe close` → publishes `pipe.closed{closed}`;
- Presenter process exit via Ctrl-C (SIGINT) or `kill` (SIGTERM) → publishes
  `pipe.closed{owner_exit}`;
- a **hard kill (SIGKILL) / power loss** cannot publish anything: forwarding still stops when the
  presenter's endpoint dies, but the pipe shows **open** in `pipe list` until an owner/admin
  `pipe close`. Document this as the known reachability bound (PRD §13.2), not a bug.

**A5. Verify the pipe is gone:**

```bash
iroh-rooms pipe list <ROOM_ID>
```

Expected `(no open pipes)` once the close has synced locally.

### 4.5 Scenario B — an agent-generated preview

This satisfies the scope item "Agent-generated preview scenario **if example agent supports
it**." **There is no dedicated example-agent binary in this repo** — an "agent" is an ordinary
principal that holds its own identity key and was invited with the agent role (see
`getting-started.md` Step 3 "Invite and join the Agent"). Document the scenario honestly on that
basis rather than inventing a tool:

- Frame: an agent building a site produces a static preview directory (e.g. `./preview-build/`)
  and exposes it to a human reviewer for sign-off — no deploy, no public URL.
- Because the agent is a first-class principal, the mechanics are **identical to Scenario A**,
  run from the agent's terminal / home. Show it concretely and note only the deltas:

```bash
# Agent terminal (the agent is an active member invited with --role agent).
python3 -m http.server 3000 --directory ./preview-build
iroh-rooms pipe expose <ROOM_ID> --tcp 127.0.0.1:3000 --allow <REVIEWER_ID> --label agent-preview
```

- Reviewer connects exactly as in A3.
- **Security point to make explicit** (PRD §13.3.5): the agent can open a pipe **only because it
  was explicitly invited and is an active member**; a non-member agent's `pipe expose` is refused
  with `error[peer_unauthorized]` (exit 3) before any dial — the same gate as any principal. The
  agent is first-class but not implicitly trusted.
- Add a one-line honest scoping note: this repo ships no turnkey "example agent" process; the
  scenario reuses the same CLI a human uses, driven from the agent's identity. If a dedicated
  example agent lands later, link it here. (Keeps the guide truthful to the codebase.)

### 4.6 Security warning & close flow (deep dive)

Satisfies scope "Security warning and close flow." Consolidate the safety story (PRD §13.2):

- **Explicit authorization only** — `--allow` is required and repeatable; there is no
  default-all. Exposing to nobody is refused before any IO.
- **Loopback-only target** — a non-loopback `--tcp` is refused ("refusing to expose non-loopback
  target …"). The pipe forwards a *local* service; it does not bind a public port.
- **Visible trust decision** — the ⚠ SECURITY warning (stderr) names the exact target and each
  allowed member (short id); the full ids are on the `allow:` stdout lines. Reproduce a sample.
- **Least privilege / TTL** — recommend `--expires` for short-lived previews and `--allow`ing the
  single reviewer, not the whole room.
- **Clean teardown** — the three close paths from A4, plus: closing emits a `pipe.closed` event
  that other members observe; `pipe list` reflects it after sync.
- **Local audit** — reject/teardown lines (`pipe.connect.rejected:<cause>`, `pipe.torndown:<cause>`)
  are printed on the presenter's stderr and are stable/greppable; `-v` adds per-connection
  accepts. Note the CLI has **no** tracing subscriber, so this stderr sink is the audit surface
  the operator actually sees.

### 4.7 Unauthorized access behavior

Satisfies **AC "Unauthorized access behavior is documented."** Cover the two distinct rejection
cases precisely (match `getting-started.md` Troubleshooting → Unauthorized peer, do not restate
differently):

1. **Room member, not in `--allow`** — the connect is rejected at connect time by the owner's
   gate. The **reviewer** sees a denied status line; the **presenter** sees, on stderr:

   ```text
   pipe.connect.rejected:not_allowed peer=<ep-8-hex> pipe=<pipe-8-hex>
   ```

   The reviewer's connector reports the outcome (`[pipe] denied by the owner (not authorized /
   closed)`). No traffic flows.

2. **Not an active member of the room at all** — turned away **locally, before any dial**:
   `pipe connect` (and `pipe expose` / `pipe close`) report
   `error[peer_unauthorized]: you are not an active member of room …` and exit `3`.

Also list the other owner-side reject causes the operator may see and what each means
(`not_active`, `closed`, `expired`, `unknown_device`, `owner_inactive`). Next action for each:
re-`expose` with the correct `--allow`, or have the admin invite the peer first. Cross-link
`getting-started.md#unauthorized-peer`.

### 4.8 Availability & relay-fallback honesty

Satisfies **AC "Availability and relay fallback limitations are stated honestly."** A dedicated,
pipe-specific honesty box (consistent with `getting-started.md` "Availability model" bullet 3
and PRD §14 / §18):

- **Both peers must be online for the entire session.** A pipe is a live stream, not a stored
  artifact; there is no cloud inbox and nothing is queued. If either peer drops, forwarding
  stops.
- **Connectivity is best-effort P2P with relay fallback.** The presenter and reviewer connect
  directly via NAT hole-punching when the network allows; otherwise they **fall back to a relay**
  (PRD §18.1; `PHASE-0-SPIKE.md` Gate A). Some networks will not permit a direct path — that is
  expected, not an error. Relay-relayed pipes work but may have higher latency.
- **An offline owner fails loudly, not silently.** `pipe connect` is the one command that fails
  when the target is unreachable: `error[peer_offline]: the pipe owner is unreachable: …`,
  exit `6`. Retry when the owner is back; nothing is queued.
- **Hard-kill reachability bound** (from §4.4): a pipe can linger as "open" in `pipe list` after a
  SIGKILL/power-loss until an owner/admin `pipe close`. Documented limit, not a leak — no traffic
  flows because the endpoint is dead.
- **No always-on infrastructure by default.** Optional user-owned infra later (an always-on node,
  a relay) can improve reachability but never owns the room.

Keep the language neutral and factual; do not oversell reliability.

### 4.9 Comparison vs. public tunnels (neutral product language)

Satisfies scope "Comparison against public tunnel workflow in neutral product language" and the
core value-prop ask (PRD §17.3.5, §2, §5.7). **Writing constraints — enforce these:**

- Compare **"a public tunnel / cloud deploy"** as a **category**. Do **not** name specific
  vendors or products, and do **not** disparage them. Neutral, factual, benefit-framed.
- Frame as "different trade-offs for different jobs," not "X is bad." Public tunnels are great for
  sharing with anyone / no shared room; Live Pipe targets *private review between people/agents
  already in a room.*
- Every row must be defensible against this codebase and PRD — no aspirational claims.

Provide a comparison table with roughly these rows (neutral wording):

| Dimension | Public tunnel / cloud preview | Live Pipe (this workflow) |
|---|---|---|
| Who can reach it | Anyone with the URL (often public by default) | Only the identities you `--allow`, who must be active room members |
| Where the URL lives | A third-party service issues a public hostname | No public hostname; addressed by room + pipe id, peer-to-peer |
| Auth model | Tunnel-provider account / link-holder | Room membership + explicit per-member allow-list, signed events |
| Data path | Traverses the provider's servers | Direct P2P when possible, else a relay; loopback target never leaves your machine except through the authenticated pipe |
| Lifetime | Until you stop the tunnel / the link expires | Session-lived; closes on `pipe close`, owner exit, or `--expires` |
| Availability | Provider-hosted, works while their edge is up (even if you're offline) | Requires both peers online; no cloud inbox, no queued delivery |
| Audit | Provider dashboard/logs | Local, greppable audit lines on the owner's terminal |
| Best for | Sharing broadly, webhooks, demos to unknown parties | Private preview review with specific people/agents already in a room |

Follow the table with 2–3 sentences: choose a public tunnel when you need a broadly reachable
URL or the reviewer isn't in your room; choose Live Pipe when the review is private, scoped to
named peers, and you'd rather not put an intermediate build on someone else's infrastructure.
This is the "meaningfully different from chat / preferred over public tunnels" validation the PRD
measures (§17.3.2, §17.3.5).

### 4.10 Quick CLI reference (appendix)

A compact reference table of the four subcommands and their flags, taken verbatim from
`cli.rs`:

- `pipe expose <ROOM_ID> --tcp <IP:PORT> --allow <ID>… [--label <L>] [--expires <int>{s|m|h|d}] [--peer <EP>[@ip:port]]… [-v]`
- `pipe connect <ROOM_ID> <PIPE_ID> --local <PORT> [--peer <EP>[@ip:port]]…`
- `pipe close <PIPE_ID> [--room <ROOM_ID>] [--peer …]`
- `pipe list <ROOM_ID>` (offline)

Note `--loopback` is a hidden CI/test flag; do not surface it in the operator-facing body.

### 4.11 Troubleshooting cross-links

Short pointers (do not duplicate the tables): link to `getting-started.md#offline-peer`,
`#unauthorized-peer`, and the "Stable error/warning lines and exit codes" section. List the three
pipe-relevant exit codes in one line: `peer_unauthorized`=3, `peer_offline`=6, and successful
runs=0.

### 4.12 References footer

Link `PRD.v0.3.md` (§9.3, §13.2, §16, §17.3, §18), `docs/getting-started.md` (Steps 1–3 setup,
Step 6 in-demo pipe), `PHASE-0-SPIKE.md` §5 (pipe/blob authorization) and Gate A (NAT/relay),
and the pipe specs (`specs/live-tcp-pipe-path.md`, `specs/authenticated-tcp-pipe-expose-connect-close.md`).

---

## 5. Cross-linking / index edits (small, required)

So the guide is discoverable (docs-only edits, no code):

1. **`docs/getting-started.md`** — in Step 6 add a one-line pointer ("For a task-focused guide
   with an agent scenario and a public-tunnel comparison, see
   [`live-pipe-preview.md`](./live-pipe-preview.md)"), and add the guide to the "Next steps &
   references" list.
2. **`README.md`** — if it links docs (verify during implementation), add
   `docs/live-pipe-preview.md` to the docs list with a one-line hook.
3. Ensure relative links resolve (`docs/` → sibling files use `./`, repo-root files use `../`).

Do **not** edit any `.rs`, `Cargo.toml`, PRD, or spec files' semantics.

---

## 6. Out of scope

- Any change to pipe CLI behavior, flags, output strings, gate logic, or audit vocabulary. If the
  guide and the code disagree, the **code wins** and the guide is corrected — never the reverse in
  this work item.
- A new example-agent binary (§4.5 documents the scenario with the existing CLI honestly instead).
- Screenshots / recorded terminal casts (optional, not required by ACs).
- Translations, marketing copy beyond the neutral comparison.

---

## 7. Verification / test plan

Per the issue Test Plan ("Run guide with a local HTTP server and verify expected CLI outputs"):

### 7.1 Manual runbook (authoritative acceptance check)

On one host, using the loopback/CI stack (`--loopback`) or a real network with `--peer`:

1. Follow `getting-started.md` Steps 1–3 to get a room with two active members (Alice + Bob),
   plus an agent identity for Scenario B.
2. Execute **Scenario A** (§4.4) verbatim from the guide. Confirm:
   - the ⚠ SECURITY lines appear on **stderr** and the summary on **stdout**;
   - `curl http://localhost:3001` returns the presenter's served content (authorized view works);
   - `pipe close` prints `closed pipe …` and `pipe list` then shows `(no open pipes)`.
3. Execute **Scenario B** (§4.5) from the agent identity; confirm the reviewer sees the
   agent-served content.
4. Execute the **unauthorized cases** (§4.7): a member not in `--allow` → owner stderr
   `pipe.connect.rejected:not_allowed`, reviewer sees denied; a non-member →
   `error[peer_unauthorized]` exit 3. Verify exit codes with `echo $?`.
5. Execute the **offline** case: stop the presenter, run `pipe connect` → `error[peer_offline]`
   exit 6.

Every "Expected output" block in the guide must match observed output (allowing for the
documented placeholders: ids, ports, hashes). Where reality differs, fix the **guide** to match
the CLI.

### 7.2 Regression backing (no new prod tests required)

- The authorized/unauthorized live-pipe pair is already covered by the `#[ignore]`-gated
  integration test referenced in `getting-started.md` "Status of this guide." Re-run it locally
  (the command is quoted in that status section) to confirm the flow the guide narrates still
  holds. Cite it in §4.12 so readers can self-verify.
- **Optional hardening (nice-to-have, not required by ACs):** if the repo has (or later adds) a
  doc-transcript check that greps guide code fences against real CLI runs, register the new
  guide's Scenario-A commands there. Do not build such a harness solely for this item.

### 7.3 Quality gate

- Run `scripts/verify.sh` if it touches docs (it should pass trivially — no Rust changed). Markdown
  links must resolve.
- Run the repo's writing-style review if available; keep the comparison section neutral (no vendor
  names, no disparagement) as a hard review criterion.

---

## 8. Acceptance criteria → where satisfied

| Issue AC | Satisfied by |
|---|---|
| Guide shows expose/connect/close flow clearly | §4.4 (A2–A5) + §4.6 |
| Authorized peer can view a local preview | §4.4 A3 (`curl` over the pipe) + §7.1 step 2, backed by the authorized live-pipe e2e |
| Unauthorized access behavior is documented | §4.7 (both member-not-allowed and non-member cases) + §7.1 step 4 |
| Availability and relay fallback limitations are stated honestly | §4.8 (dedicated pipe honesty box) + §4.9 availability row |
| (Scope) Local web server preview scenario | §4.4 |
| (Scope) Agent-generated preview scenario | §4.5 (honest: reuses CLI, no example-agent binary) |
| (Scope) Security warning and close flow | §4.6 |
| (Scope) Neutral public-tunnel comparison | §4.9 |

---

## 9. Risks

1. **Doc/code drift.** Output strings (warning wording, next-step hints, exit codes) are copied
   from today's `pipe.rs`. *Mitigation:* the guide quotes exact strings and §6 makes "code wins"
   the rule; the §7.1 runbook catches drift on each run.
2. **Overclaiming the comparison.** A tunnel comparison can slide into marketing or unfairness.
   *Mitigation:* §4.9 hard constraints (category not vendor, neutral, every row PRD/code-defensible)
   plus a writing-review gate in §7.3.
3. **Agent scenario implies a tool that doesn't exist.** *Mitigation:* §4.5 explicitly documents
   that the scenario reuses the ordinary CLI from an agent identity and states no example-agent
   binary ships.
4. **Redundancy with Step 6.** Two pipe docs could diverge. *Mitigation:* §3 single-source rule —
   the guide links Step 6 for the in-demo minimal path and owns the task/comparison narrative;
   shared facts are phrased consistently.
5. **Environment-dependent connectivity in the runbook.** Direct vs. relay varies by network.
   *Mitigation:* §4.8 frames relay fallback as expected; the runbook uses `--loopback` or `--peer`
   for determinism.

---

## Assumptions

- **Dependency mapping (#23, #34, #35).** GitHub is not reachable in this phase; from repo state I
  take these to be the landed pieces this guide sits on: the live TCP pipe **net path** (gate /
  audit — `specs/live-tcp-pipe-path.md`), the pipe **CLI** commands (`expose|connect|close|list`
  in `crates/iroh-rooms-cli/src/pipe.rs`), and the **getting-started demo** (Step 6 + agent
  identity). The spec depends only on the *landed behavior*, not on the exact issue numbers, so a
  mismatch in this mapping does not change the deliverable.
- The `iroh-rooms` binary and its pipe output strings are as read from `pipe.rs` / `cli.rs` at the
  current HEAD; the guide must be validated against the binary at implementation time (§7.1).
- The demo's `#[ignore]`-gated live-pipe e2e still exists and passes locally; it is the regression
  anchor, so no new production test is required for a docs item.
- Neutral-language comparison (no vendor names) is the intended reading of "neutral product
  language."

## Open questions

1. **File placement/name** — `docs/live-pipe-preview.md` assumed. If the team prefers a
   `docs/guides/` subdir or a different slug, adjust the one link edit in §5. (Recommend keeping it
   flat next to `getting-started.md`.)
2. **Depth of the agent scenario** — document-only (as specified here), or also add a tiny
   `--directory ./preview-build` convenience wrapper/example? This spec keeps it docs-only to
   honor "do not implement." Flag if a runnable example agent is desired as a follow-up item.
3. **Doc-transcript automation** — is there appetite to gate guide code fences against real CLI
   output in CI (§7.2 optional), or is the manual runbook sufficient for a p1/low docs item?
   (Recommend manual for now.)
4. **README linkage** — confirm `README.md` is the right index to advertise the guide, vs. only
   the demo's "Next steps."
