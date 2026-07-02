# Developer Preview Release-Readiness Checklist (IR-0306 / #41)

| Field | Value |
| --- | --- |
| **Issue** | #41 — [IR-0306] Add developer preview release-readiness checklist |
| **Parent** | #4 (Phase 2 — Developer Preview epic) |
| **Labels** | type/docs, type/security, area/dx, priority/p1, risk/low |
| **Traceability** | `PRD.v0.3.md` §13 (Security and Privacy Model), §17.2 (Developer Experience Metrics), §18 (Risks); §14 (Availability), §19 (Phase 2 Roadmap) |
| **Dependencies** | #35 / IR-0210 (getting-started demo), #37 / IR-0302 (implementer protocol docs), #38 / IR-0303 (CLI error handling + diagnostics), #39 / IR-0304 (example agent), #40 / IR-0305 (dev-preview Live Pipe guide) — **all landed** |
| **Kind** | Documentation + a thin executable gate + a deterministic conformance test. No production/runtime code, no protocol/event-schema change. |
| **Status** | Planning (this document). Implementation not started. |

---

## 1. Goal

Add a **lightweight, repeatable release-readiness checklist** that a maintainer runs
against any candidate developer-preview build before declaring it ready. The checklist
must:

- Enumerate the **required tests** (protocol, integration, pipe security, blob
  verification, agent flow) and separate the ones that **block release (P0)** from the
  ones that are **tracked-but-not-blocking (P1)**.
- List the **known MVP limitations** explicitly, so a preview ships honest.
- Surface the **security warnings** a preview must reproduce (pipe exposure, tickets as
  capabilities, unencrypted local storage, agent trust posture).
- Include a **dependency/churn review** step.
- Include a **demo verification** step (dry-run `docs/getting-started.md` against the
  candidate build).
- Provide a **release-notes template**.
- Make it **structurally impossible to mark a preview ready while P0 tests are failing**
  (the load-bearing acceptance criterion — mechanized, not honor-system).

This mirrors, at the preview cadence, what `PHASE-0-GO-NO-GO.md` did once at the end of
Phase 0: a single traceable artifact that turns "is it ready?" into a checklist with an
enforceable gate.

## 2. Non-goals

- No new runtime capability, CLI command, event type, wire-format, or migration.
- Not a CI change to the *default* pipeline. `scripts/verify.sh` (the PR/`main` gate)
  stays exactly as it is. The new release gate is a **separate, manually-invoked**
  script run at release time (it drives the flaky, resource-heavy `#[ignore]`-gated
  online tiers that must never run in the ordinary PR gate).
- Not a substitute for the Phase-0 go/no-go memo or the Gate-E MVP verdict (#15). The
  preview checklist references those; it does not re-decide them.
- No attempt to *fix* any known limitation (Gate A, live-tail display gap, storage
  encryption, etc.). Those are listed, not resolved.

## 3. Current state — what this composes (already landed)

Everything the checklist gates already exists; this issue assembles it into one gate.
Key inputs, verified in-repo:

- **The real CI gate** is `scripts/verify.sh` (invoked by `.github/workflows/verify.yml`):
  `cargo fmt --all --check` → `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  → `cargo test --workspace --all-targets --all-features` → `cargo test -p iroh-rooms --doc`
  → `cargo build -p iroh-rooms --examples`. Passing `cargo test` alone is **not** CI-green
  (fmt + pedantic clippy also gate).
- **Protocol tests** — `crates/iroh-rooms-core/tests/protocol_conformance.rs` (the §1–§20
  conformance binary), `conformance/*` (serialization, membership, taxonomy completeness,
  idempotency/ordering, docs-reference), `golden_vectors.rs`, `cbor_property.rs`
  (proptest), `membership_fold.rs`.
- **Integration tests** — `crates/iroh-rooms-cli/tests/two_peer_e2e.rs` (Phase 1A) and
  `full_demo_e2e.rs` (Phase 1B, two humans + one agent). Each is **tiered**: a
  deterministic CI tier that always runs in `cargo test`, plus an `#[ignore]`-gated
  loopback online tier run with `-- --ignored --test-threads=1`.
- **Pipe security** — `crates/iroh-rooms-net/tests/pipe_e2e.rs` (P1–P6),
  `crates/iroh-rooms-cli/tests/pipe_cli.rs`, plus the unauthorized-denial split proven in
  `two_peer_e2e.rs` / `full_demo_e2e.rs`.
- **Blob verification** — `crates/iroh-rooms-net/tests/blob_e2e.rs`, `file_e2e.rs`,
  `crates/iroh-rooms-cli/tests/file_cli.rs`,
  `crates/iroh-rooms-core/tests/file_shared_hashes.rs` (BLAKE3-256 verify, two-gate ACL,
  hash-mismatch hard stop).
- **Agent flow** — `crates/iroh-rooms-cli/tests/agent_cli.rs`, `agent_e2e.rs`,
  `agent_invite_flow.rs`, and `crates/iroh-rooms/tests/example_agent_e2e.rs` (IR-0304).
- **Doc/demo conformance already exists as a pattern** — `docs_conformance.rs` (74
  structural tests over `docs/getting-started.md`), `live_pipe_preview_docs.rs` (23),
  `phase0_memo_conformance.rs` (20). These read Markdown and assert structure/commands;
  the new checklist conformance test follows this exact idiom.
- **The manual gated online tiers** and their exact commands are documented in the README
  ("Run the gated online tier locally …").
- **Gate A (real-NAT hole-punching)** is still **PENDING** — the `nat-probe` harness
  (`crates/spike-nat`, IR-0012) is CI-proven on loopback but the two-host cross-network run
  and its go/no-go verdict are owed (feeds Gate E / #15). See `crates/iroh-rooms-net/NOTES.md`.

## 4. Owning artifact & placement decision

**Decision: a root-level `RELEASE-READINESS.md`**, parallel to `PHASE-0-GO-NO-GO.md` and
`PHASE-0-SPIKE.md`. Rationale: it is a maintainer-facing, per-build release gate (not a
tutorial), and the repo already keeps its process/gate memos at the root; discoverability
is highest there. (`docs/` is reserved for user/developer walkthroughs — getting-started,
protocol, live-pipe-preview — which this is not.) See [Open Questions](#14-open-questions)
for the `docs/release-readiness.md` alternative.

Three artifacts, in priority order:

| Artifact | Path | Purpose | Blocks CI? |
| --- | --- | --- | --- |
| **D1** Checklist doc | `RELEASE-READINESS.md` (root) | The checklist, known limitations, security warnings, dependency/churn step, demo-verification step, and the release-notes template. Single source of truth. | Indirectly (via D3) |
| **D2** Release gate script | `scripts/release-readiness.sh` | Executable P0 gate. Runs the deterministic P0 set + the P0 online tiers, and prints the `READY`/`NOT READY` verdict from real exit codes. **Mechanizes AC4.** | No (manual, release-time) |
| **D3** Conformance test | `crates/iroh-rooms-cli/tests/release_readiness_docs.rs` | Deterministic structural test: the doc exists, has every required section, the P0 command list in the doc matches the commands in D2, and the release-notes template is present. Runs inside `scripts/verify.sh`. | Yes (part of `cargo test`) |

D2 is the load-bearing piece for AC4; D1 is the human-readable contract; D3 keeps D1 and
D2 from drifting.

## 5. P0 vs P1 test taxonomy (the checklist's core table)

The checklist defines **P0 = release-blocking**, **P1 = tracked, must be explicitly
acknowledged but not auto-blocking for a *developer preview***. AC1's five required areas
each map to concrete commands. This table is embedded verbatim in `RELEASE-READINESS.md`
and asserted by D3.

### 5.1 P0 — deterministic (always run; this is `scripts/verify.sh`)

| Area (AC1) | Command / suite | Notes |
| --- | --- | --- |
| Toolchain hygiene | `cargo fmt --all --check` | first gate |
| Toolchain hygiene | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pedantic; the memory "verify.sh is the real CI gate" applies |
| Protocol tests | `cargo test -p iroh-rooms-core --test protocol_conformance --all-features` | §1–§20 vectors, taxonomy-completeness gate, golden vectors |
| Protocol tests | `cargo test -p iroh-rooms-core --test cbor_property` / `golden_vectors` / `membership_fold` | strict-reader fuzz, byte-exact goldens, fold determinism |
| Integration (CI tier) | `cargo test -p iroh-rooms-cli --test two_peer_e2e` / `--test full_demo_e2e` | deterministic, network-free backbone |
| Blob verification | `cargo test -p iroh-rooms-net --test blob_e2e --test file_e2e` + `cargo test -p iroh-rooms-cli --test file_cli` | BLAKE3 verify, ACL, hash-mismatch |
| Agent flow (CI tier) | `cargo test -p iroh-rooms-cli --test agent_cli --test agent_invite_flow` | offline matrix + ticket-rejection legs |
| SDK surface | `cargo test -p iroh-rooms --doc` + `cargo build -p iroh-rooms --examples` | façade doctests + example builds |
| Full workspace | `cargo test --workspace --all-targets --all-features` | superset that also runs pipe/manager/store/sync suites |

The whole of 5.1 is exactly what `scripts/verify.sh` runs, so D2 invokes `scripts/verify.sh`
rather than re-listing commands (single source of truth; no drift).

### 5.2 P0 — gated online tiers (loopback; must be run on the candidate build)

These are `#[ignore]`-gated (flaky/resource-heavy) and therefore excluded from the PR CI
gate, but they prove product-level ACs and are **release-blocking**. Run with
`-- --ignored --test-threads=1`:

| Area (AC1) | Command |
| --- | --- |
| Integration + membership convergence | `cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1` |
| Full demo (2 humans + 1 agent) + demo verification | `cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1` |
| Pipe security (authorized + unauthorized live) | `cargo test -p iroh-rooms-cli --test pipe_cli -- --ignored --test-threads=1` |
| Agent flow (live status push) | `cargo test -p iroh-rooms-cli --test agent_e2e -- --ignored --test-threads=1` |
| Agent flow (example agent, IR-0304) | `cargo test -p iroh-rooms --test example_agent_e2e -- --ignored --test-threads=1` |
| Error taxonomy / diagnostics (live) | `cargo test -p iroh-rooms-cli --test error_taxonomy_e2e --test diagnostics_cli -- --ignored --test-threads=1` |
| SDK façade over real QUIC | `cargo test -p iroh-rooms --test facade_e2e -- --ignored --test-threads=1` |

> Implementation note for D2: discover the online-tier test binaries dynamically or keep
> this list in one array in the script; D3 asserts the doc's 5.2 list and the script's
> array are identical (byte-for-byte on the command strings) so they cannot drift.

### 5.3 P1 — tracked, requires explicit acknowledgement (not auto-blocking)

| Item | Why not auto-P0 | Where recorded |
| --- | --- | --- |
| **Gate A real-NAT run** (`nat-probe`, two hosts on different networks, both directions, natural + `--relay-only`) | Requires two real hosts; cannot run in a single-machine script. For a *developer preview* it is a documented residual risk, not a blocker — but the maintainer must record its current status and paste the latest `crates/spike-nat/results/results.md`. | Checklist "Known limitations" + a required "Gate A status:" field |
| **Live-tail display gap** — streaming `room tail` renders only `message.text` (agent.status/file rows appear only under `--offline`) | Cosmetic; offline read is complete | Known limitations |
| **DX metric timings** (PRD §17.2) — first identity <1 min, two-peer room <3 min, first pipe <5 min | Human-timed, environment-dependent | Demo-verification section (record measured values) |

## 6. Deliverable D1 — `RELEASE-READINESS.md` structure

The doc is a fill-in-per-build checklist. Required top-level sections (D3 asserts each
heading exists, matched case-insensitively on a stable anchor slug):

1. **`## How to use this checklist`** — one paragraph: run `scripts/release-readiness.sh`;
   a preview is READY only when that script exits `0`; hand-ticking boxes is not a
   substitute for the gate. States the P0/P1 distinction.
2. **`## Candidate build`** — fields to fill: commit SHA, date, rustc/toolchain version,
   platform(s) exercised.
3. **`## P0 required tests`** — embeds the §5.1 + §5.2 tables and a single instruction:
   `scripts/release-readiness.sh` runs all of it; paste the final verdict line.
4. **`## Pipe security review`** — checkboxes traceable to PRD §13.2: `--allow` required,
   no default all-member exposure, loopback-only bind, §13.2.4 warning names target +
   each allowed member, `pipe.closed` on clean exit, local audit vocabulary present.
5. **`## Blob verification review`** — checkboxes: BLAKE3-256 recompute on fetch, two-gate
   ACL (member + referenced-hash), hash-mismatch is a hard stop (no provider fallthrough),
   path-traversal-safe basename on save.
6. **`## Agent flow review`** — checkboxes traceable to PRD §13.3: agent has own identity,
   joins only via explicit invite, events signed, least-privileged `agent` role, no
   implicit room access, artifacts content-addressed/verified.
7. **`## Known MVP limitations`** — the explicit list (see §7 below). AC2.
8. **`## Security warnings`** — the explicit list (see §8 below). Part of AC (type/security).
9. **`## Dependency / churn review`** — the steps in §9 below.
10. **`## Demo verification`** — dry-run `docs/getting-started.md` against the candidate
    build (the automated proxy is `full_demo_e2e.rs`'s `full_demo_two_humans_one_agent`);
    record the three PRD §17.2 timings; confirm a developer can complete the demo without
    maintainer help.
11. **`## Release notes template`** — the fenced template (see §10 below). AC3.
12. **`## Sign-off`** — verdict (`READY` / `NOT READY`), the pasted
    `release-readiness: …` line from D2, Gate-A status, reviewer, date.

## 7. Known MVP limitations to enumerate (AC2)

Drawn from `PRD.v0.3.md` §13.4 / §14, the README status log, and the crate NOTES. The doc
lists these explicitly (D3 asserts the section is non-empty and mentions the starred items):

- **★ No verified real-NAT connectivity yet (Gate A pending).** Direct hole-punching on
  restrictive/symmetric networks is unproven; relay fallback exists but the cross-network
  measurement + verdict are owed. (`crates/iroh-rooms-net/NOTES.md`, PRD §18.1)
- **★ No cloud inbox; no guaranteed offline delivery.** Files/pipes require a provider
  online; messages deliver only when peers are online or reconnect. (PRD §14)
- **No group E2EE, no PFS, no advanced key rotation, no secure multi-device recovery.**
  (PRD §13.4 items 1–4)
- **No invite revocation; weak protection after a ticket leak.** A ticket is a scoped
  capability until it expires or is consumed. (PRD §13.4 item 10, §13.5 item 1)
- **Unencrypted local storage.** `rooms.db` / `blobs/` are plaintext on disk. (PRD §13.4
  item — storage encryption is roadmap §13.5 item 9)
- **Join-bootstrap privacy trade-off (Approach A).** With `--accept-joins`, a dialer who
  knows `room_id` + admin `EndpointId` is served the **secret-free** membership sub-DAG
  during the open-invite window; no capability secret is disclosed, and the window closes
  when the flag is off. (IR-0104)
- **Live-tail display gap.** Streaming `room tail` renders only `message.text`;
  `agent.status`/`file.shared` rows show only in `room tail --offline`.
- **SIGKILL leaves a pipe open on the log** until an owner/admin `pipe close` (clean
  SIGINT/SIGTERM emit `pipe.closed{owner_exit}`). (IR-0108)
- **CLI installs no tracing subscriber.** Audit is only via the explicit stderr sinks
  (`pipe.*`, `reject.*`); `Tracing*Audit` output is dropped. (project memory
  "CLI has no tracing subscriber")
- **Small-room, single-immutable-admin, TCP-only-pipe, loopback-only-bind** scope. (PRD
  §7, §18.4)
- **File-size cap divergence to confirm.** Code enforces `MAX_SHARED_FILE_BYTES` = **100
  MiB**; PRD §17.1 metric target is **25 MB**. The checklist records the shipped cap and
  flags the divergence rather than silently accepting it.

## 8. Security warnings the preview must reproduce (type/security AC)

The checklist confirms the *shipping build actually prints/enforces* each of these (many
are already covered by tests; the checklist is the human cross-check):

- **Pipe exposure warning** — `pipe expose` prints the PRD §13.2.4 warning naming the
  exposed target and **each** allowed member, before forwarding. (proven by `pipe_cli.rs`)
- **Ticket = password-grade capability** — `room invite` output ends with the
  password-grade warning; a ticket/secret never appears in any error or audit line.
  (proven by `invite_cli.rs`, error-taxonomy tests)
- **Loopback-only bind** — non-loopback `--tcp` targets are refused; the connector binds
  `127.0.0.1` only. (proven by `pipe_cli.rs` / `pipe_e2e.rs`)
- **Agents are not implicitly trusted** — least-privileged `agent` role; no room access
  without an explicit key-bound invite. (PRD §13.3; proven by `agent_invite_flow.rs`)
- **Local-storage plaintext disclaimer** — the preview must state storage is unencrypted
  (there is no code warning today; this is a doc-level disclosure requirement).

## 9. Dependency / churn review step

Checklist steps for the maintainer (no automation required beyond the commands; these are
guidance, run once per candidate):

- `cargo tree --workspace --edges normal` — confirm **no new runtime dependency** slipped
  into the shipping crates (`iroh-rooms-core/-net/-cli/iroh-rooms`). Dev-only additions
  (e.g. `proptest` in `iroh-rooms-core` `[dev-dependencies]`) are fine.
- Confirm the **iroh pin is unchanged** (1.0.1) or, if bumped, that the "no `ConnectionType`
  watcher" workaround in `spike-nat`/diagnostics still holds. (project memory
  "iroh 1.0.1 has no ConnectionType watcher")
- Confirm the **spike crates stay isolated**: `spike-blobs`/`spike-nat`/`spike-transport`
  remain `publish = false`, off the shipping dependency tree, and referenced only as
  throwaway harnesses.
- `cargo update --dry-run` (or a lockfile diff vs. the previous preview tag) — eyeball the
  churn; flag any transitive major bumps.
- Cross-reference `PHASE-0-GO-NO-GO.md` §5 "Pinned dependency observations" — confirm none
  of the recorded caveats regressed.

## 10. Release-notes template (AC3)

Embedded as a fenced ```` ```markdown ```` block inside `RELEASE-READINESS.md` under
`## Release notes template`. D3 asserts the block exists and contains the required
placeholders. Proposed content:

```markdown
# Iroh Rooms — Developer Preview <VERSION> (<DATE>)

Status: DEVELOPER PREVIEW. Not for production. No security audit has been performed.

## What you can do
- <one-line per shipped capability: identity, room, invite/join, message, file share/fetch,
  live pipe, agent status, Rust SDK>

## Highlights since <PREV_VERSION>
- <notable changes>

## Known limitations (read before relying on this)
- No verified real-NAT connectivity yet (Gate A pending): <current status>.
- No cloud inbox / no guaranteed offline delivery; peers must be online.
- Local storage is unencrypted; no invite revocation; no group E2EE / PFS.
- <copy the full list from RELEASE-READINESS.md "Known MVP limitations">

## Security notes
- Live Pipe exposes a local TCP service to an explicitly authorized peer only.
- Invite tickets are scoped capabilities — treat them like passwords.
- Agents join only via explicit invite and run at the least-privileged role.

## Verified on this build
- P0 gate: <paste the `release-readiness: READY` line from scripts/release-readiness.sh>
- Platforms exercised: <os/arch list>
- Demo (docs/getting-started.md) dry-run: <PASS/FAIL>, timings: identity <t1>, room <t2>, pipe <t3>

## Install / run
- <build + run instructions, cross-linked to README + docs/getting-started.md>
```

## 11. Deliverable D2 — `scripts/release-readiness.sh` (mechanizes AC4)

A bash script (`set -euo pipefail`, matching `verify.sh` style) that computes the verdict
from real exit codes, so **a `READY` verdict is unreachable while any P0 test fails**.

Behaviour:

1. Run `scripts/verify.sh` (the entire §5.1 deterministic P0 set). Capture pass/fail.
2. Unless `--skip-online` is passed, run each §5.2 gated online tier in sequence, each
   with `-- --ignored --test-threads=1`. Capture per-suite pass/fail.
   - `--skip-online` prints a loud, non-suppressible
     `release-readiness: ONLINE TIER SKIPPED — NOT release-ready` and forces a non-zero
     exit, so skipping can never masquerade as ready (it exists only for iterating on the
     deterministic tier locally).
3. Print a per-check summary table, then a single machine-parseable verdict line:
   - all P0 green → `release-readiness: READY` and `exit 0`.
   - any P0 red → `release-readiness: NOT READY (<n> P0 checks failing: <names>)` and
     `exit 1`.
4. **P1 items are surfaced but never flip the verdict**: the script prints
   `gate-a: <reads crates/spike-nat/results/results.md presence>` and a reminder line, but
   Gate A never blocks (it can't run here).

The script does **not** run in `verify.yml` (it would drag the flaky online tiers into the
PR gate). It is invoked manually at release time and its verdict line is pasted into the
checklist's Sign-off section.

> Why this satisfies AC4 precisely: "marked ready" is operationally defined as "the
> `release-readiness: READY` line was produced." That line is emitted only on the
> all-green branch, which is reached only when every P0 exit code was `0`. There is no code
> path that prints `READY` with a failing P0 test. The doc's Sign-off section requires the
> pasted line, and D3 asserts the doc says READY requires an exit-0 gate — closing the loop
> between the human checklist and the machine gate.

## 12. Deliverable D3 — `release_readiness_docs.rs` (deterministic, in `verify.sh`)

Following the `docs_conformance.rs` / `phase0_memo_conformance.rs` / `live_pipe_preview_docs.rs`
idiom (read the Markdown, assert structure — no network, no binary exec):

- `RELEASE-READINESS.md` exists at the workspace root and is non-empty.
- Every §6 required section heading is present.
- The "How to use" / Sign-off sections state that READY requires
  `scripts/release-readiness.sh` to exit `0` (ties the doc to AC4's mechanism).
- The P0 online-tier command list embedded in the doc (§5.2) is **exactly** the command
  set the script runs — assert by parsing both and comparing sets, so a renamed/added
  online test that the script picks up but the doc omits (or vice-versa) fails CI. (This is
  the anti-drift guard; cf. the memory "IR-0304 example agent scope" untimed-build pattern
  for keeping docs and commands in lockstep.)
- The `## Known MVP limitations` section is non-empty and mentions the starred items
  (Gate A, no-offline-delivery).
- The `## Release notes template` fenced block exists and contains the required
  placeholders (`<VERSION>`, the "Known limitations" heading, the "P0 gate:" line).
- `scripts/release-readiness.sh` exists and is executable (`0o111` bit) — a metadata
  assertion, not an execution.

Keep it deterministic and fast; it must pass with zero network and no state, exactly like
its siblings.

## 13. Implementation steps (ordered, for the executing engineer/agent)

1. **Write `RELEASE-READINESS.md`** (D1) at the repo root with the §6 section skeleton,
   the §5.1/§5.2 tables, the §7 known-limitations list, the §8 security-warnings list, the
   §9 dependency/churn steps, the §10 release-notes template, and the Sign-off section.
   Cross-link `PHASE-0-GO-NO-GO.md`, `docs/getting-started.md`, and
   `crates/iroh-rooms-net/NOTES.md`.
2. **Write `scripts/release-readiness.sh`** (D2). Reuse `verify.sh` for the deterministic
   tier; keep the online-tier command list in a single array; implement `--skip-online`,
   the summary table, and the verdict line. `chmod +x`.
3. **Write `crates/iroh-rooms-cli/tests/release_readiness_docs.rs`** (D3) mirroring
   `phase0_memo_conformance.rs`'s workspace-root resolution and Markdown-reading helpers.
4. **Cross-link** from `README.md` (add a "Release readiness" bullet near the "Verify"
   section pointing at `RELEASE-READINESS.md` + `scripts/release-readiness.sh`) and from
   `CONTRIBUTING.md` (a "Cutting a developer preview" subsection).
5. **Run `scripts/verify.sh`** — D3 must pass; fmt + clippy clean.
6. **Dry-run the gate** (the issue Test Plan): run `scripts/release-readiness.sh` against
   the current `HEAD` as the "first MVP candidate build". Expect a `READY` line if the
   online tiers pass on loopback; capture the output and paste it into the checklist's
   Sign-off as the worked example. If any online tier is red on this machine, that is a
   real finding — record it, do **not** weaken the gate to make it pass.

## 14. Test / verification plan (issue Test Plan: "dry-run against first MVP candidate")

- **Deterministic (in `verify.sh`):** `release_readiness_docs.rs` passes — the doc,
  script, template, and command-set-match assertions all hold.
- **Gate dry-run:** `scripts/release-readiness.sh` on `HEAD` produces a verdict line. Two
  cases to demonstrate in the PR description:
  - Happy path — all P0 green → `release-readiness: READY`, exit 0.
  - Forced-failure sanity check — temporarily break one P0 test (locally, not committed)
    and confirm the script prints `NOT READY` and exits non-zero, proving AC4 is real and
    not vacuous.
- **`--skip-online` sanity:** confirm it prints the loud SKIPPED line and exits non-zero.
- **Demo verification:** confirm `full_demo_e2e.rs`'s online tier (the automated proxy for
  the `docs/getting-started.md` dry-run) is in the P0 online set and passes.
- **No regression:** `scripts/verify.sh` is byte-unchanged in behaviour for existing
  targets; `verify.yml` is untouched.

## 15. Acceptance criteria → where satisfied

| Issue AC | Satisfied by |
| --- | --- |
| Checklist covers protocol tests, integration tests, pipe security, blob verification, and agent flow | §5.1/§5.2 tables in D1 (all five areas mapped to concrete commands); D3 asserts the sections exist |
| Known MVP limitations are explicitly listed | D1 `## Known MVP limitations` (§7); D3 asserts non-empty + starred items |
| Release notes template exists | D1 `## Release notes template` fenced block (§10); D3 asserts presence + placeholders |
| Preview cannot be marked ready while P0 tests are failing | D2 `scripts/release-readiness.sh` computes the `READY` verdict only from all-green P0 exit codes; D1 Sign-off requires the pasted verdict line; D3 asserts the doc ties READY to an exit-0 gate (§11) |
| (label type/security) Security warnings covered | D1 `## Security warnings` (§8) + `## Pipe/Blob/Agent review` sections |
| Test Plan: dry-run against first MVP candidate | §14 (gate dry-run on `HEAD`, both happy-path and forced-failure) |

## 16. Risks

- **AC4 could be interpreted as honor-system.** Mitigated by making the READY verdict a
  script exit code (D2), not a checkbox; D3 keeps the doc honest about it.
- **Doc/command drift** — the online-tier list could diverge from reality. Mitigated by
  D3's set-equality assertion between the doc's §5.2 list and the script's array.
- **Online tiers are loopback-flaky** and slow. Mitigated by keeping them out of the PR CI
  gate (manual `release-readiness.sh` only) and serializing with `--test-threads=1`, as the
  existing suites already require.
- **Gate A ambiguity** — treating a PENDING Gate A as non-blocking for a *preview* could be
  read as hiding a gap. Mitigated by making Gate-A status a **required, explicit** field
  the maintainer must fill (not a silent omission), consistent with the repo's honesty
  posture.
- **File-cap divergence (100 MiB code vs 25 MB metric)** could confuse. Mitigated by
  listing it as an explicit known-limitation to confirm each build, not resolving it here.
- **Scope creep into a GA gate.** Mitigated by §2 non-goals — this is a preview checklist,
  not the Gate-E MVP verdict (#15).

## 17. Assumptions

- GitHub is not reachable in this phase; the dependency→IR mapping (#35 IR-0210, #37
  IR-0302, #38 IR-0303, #39 IR-0304, #40 IR-0305) was derived from spec headers and the
  README and is treated as authoritative for this plan.
- All five dependencies are landed (confirmed from README status + specs); the checklist
  gates existing behaviour and introduces no new runtime capability.
- "First MVP candidate build" = current `main`/`HEAD` of this repo; the dry-run targets it.
- The gated online tiers run on loopback only (no relay, no external tools), matching the
  existing `two_peer_e2e.rs` / `full_demo_e2e.rs` invocation contract.
- A separate manual release script (not a `verify.yml` change) is acceptable; the team does
  not want the flaky online tiers in the PR gate.

## 18. Open questions

1. **Placement:** root `RELEASE-READINESS.md` (recommended, parallels the go/no-go memo) vs
   `docs/release-readiness.md`. Plan assumes root.
2. **Release-notes template:** embedded in `RELEASE-READINESS.md` (recommended — one source
   of truth) vs a standalone `RELEASE-NOTES-TEMPLATE.md`. Plan assumes embedded.
3. **Is Gate A P0 or P1 for a developer preview?** Plan classifies it P1 (explicitly
   acknowledged, not auto-blocking), since a preview is pre-GA and Gate A can't run in a
   single-machine script. A maintainer may choose to make it a hard blocker for the *final*
   preview — the checklist should make that a conscious sign-off decision.
4. **Should `release-readiness.sh` also drive the `nat-probe` loopback self-check** (a
   weak, non-Gate-A signal that the harness still builds/runs)? Cheap to add; excluded from
   the plan to keep the gate about shipping-crate tests. Decide at implementation.
5. **File-size cap:** should this issue also reconcile the 100 MiB-vs-25 MB divergence, or
   only record it? Plan records it (reconciliation is out of scope for a docs/checklist
   issue and would touch production code).
