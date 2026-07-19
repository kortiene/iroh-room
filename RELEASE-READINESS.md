# Developer Preview Release-Readiness Checklist

| | |
|---|---|
| **Issue** | #41 — [IR-0306] Add developer preview release-readiness checklist |
| **Parent** | #4 (Phase 2 — Developer Preview epic) |
| **Traceability** | [`PRD.v0.3.md`](PRD.v0.3.md) §13 Security and Privacy Model, §17.2 Developer Experience Metrics, §18 Risks |
| **Companions** | [`PHASE-0-GO-NO-GO.md`](PHASE-0-GO-NO-GO.md) (the once-off Phase 0 gate this mirrors at preview cadence), [`docs/getting-started.md`](docs/getting-started.md) (the demo this checklist dry-runs), [`crates/iroh-rooms-net/NOTES.md`](crates/iroh-rooms-net/NOTES.md) (Gate A detail) |

---

## How to use this checklist

This is a fill-in-per-build checklist a maintainer runs against any candidate
developer-preview build before declaring it ready. It defines **P0** tests
(release-blocking) and **P1** items (tracked, must be explicitly acknowledged,
but not auto-blocking for a *developer preview*).

**A preview is READY only when `scripts/release-readiness.sh` exits `0`.**
Hand-ticking the boxes below is not a substitute for running the gate — the
script is the mechanism, this document is the human-readable record of what it
proved and what it did not. Run it from the repo root:

```bash
scripts/release-readiness.sh
```

Paste its final `release-readiness: …` verdict line into [Sign-off](#sign-off).

## Candidate build

Fill in for the build under review:

| Field | Value |
| --- | --- |
| Commit SHA | `<SHA>` |
| Date | `<YYYY-MM-DD>` |
| rustc / toolchain | `<rustc --version>` |
| Platform(s) exercised | `<os/arch, e.g. macOS 14 arm64, Ubuntu 22.04 x86_64>` |

## P0 required tests

`scripts/release-readiness.sh` runs every command below and prints a single
verdict line — paste that line into [Sign-off](#sign-off). The tables exist so
a maintainer can see *what* is covered per required area (protocol,
integration, pipe security, blob verification, agent flow) without reading the
script.

### P0 — deterministic (`scripts/verify.sh`)

The release gate's first step is exactly `scripts/verify.sh`, unchanged — no
separate command list to keep in sync here (single source of truth).

| Area | Command / suite | Notes |
| --- | --- | --- |
| Toolchain hygiene | `cargo fmt --all --check` | first gate |
| Toolchain hygiene | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pedantic; see project memory "verify.sh is the real CI gate" |
| Protocol tests | `cargo test -p iroh-rooms-core --test protocol_conformance --all-features` | §1–§20 conformance vectors, taxonomy-completeness gate, golden vectors |
| Protocol tests | `cargo test -p iroh-rooms-core --test cbor_property` / `golden_vectors` / `membership_fold` | strict-reader fuzz, byte-exact goldens, fold determinism |
| Integration (CI tier) | `cargo test -p iroh-rooms-cli --test two_peer_e2e` / `--test full_demo_e2e` | deterministic, network-free backbone |
| Blob verification | `cargo test -p iroh-rooms-net --test blob_e2e --test file_e2e` + `cargo test -p iroh-rooms-cli --test file_cli` | BLAKE3-256 verify, two-gate ACL, hash-mismatch hard stop |
| Agent flow (CI tier) | `cargo test -p iroh-rooms-cli --test agent_cli --test agent_invite_flow` | offline matrix + ticket-rejection legs |
| SDK surface (incl. real-loopback façade) | `cargo test -p iroh-rooms --doc` + `cargo build -p iroh-rooms --examples` + `cargo test -p iroh-rooms --test facade_e2e --features experimental` | façade doctests, example builds, and `facade_e2e.rs`'s four real-QUIC-loopback tests — these are **not** `#[ignore]`-gated, so `--all-features` below already runs them |
| Full workspace | `cargo test --workspace --all-targets --all-features` | superset that also runs pipe/manager/store/sync suites and `facade_e2e.rs` |

### P0 — gated online tiers (loopback)

These are `#[ignore]`-gated (they spawn real child processes / loopback
sockets, so they're excluded from the PR CI gate) but they prove
product-level acceptance criteria and are **release-blocking** for a preview.
`scripts/release-readiness.sh` runs each with `-- --ignored --test-threads=1`
unless `--skip-online` is passed (see [Sign-off](#sign-off) — skipping can
never produce a READY verdict).

> `facade_e2e.rs` (the SDK façade over real QUIC) is intentionally **not**
> listed here: its tests are not `#[ignore]`-gated and already run as part of
> the deterministic `--all-features` sweep above. Listing it again here would
> just re-run already-covered tests under a `--ignored` filter that matches
> zero tests — a vacuous, misleading "pass". This is exactly the kind of
> doc/reality drift `release_readiness_docs.rs` (D3) exists to catch.

| Area | Command |
| --- | --- |
| Integration + membership convergence | `cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1` |
| Full demo (2 humans + 1 agent) + demo verification | `cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1` |
| Pipe security (authorized + unauthorized live) | `cargo test -p iroh-rooms-cli --test pipe_cli -- --ignored --test-threads=1` |
| Agent flow (live status push) | `cargo test -p iroh-rooms-cli --test agent_e2e -- --ignored --test-threads=1` |
| Agent flow (example agent, IR-0304) | `cargo test -p iroh-rooms --features experimental --test example_agent_e2e -- --ignored --test-threads=1` |
| Error taxonomy / diagnostics (live) | `cargo test -p iroh-rooms-cli --test error_taxonomy_e2e --test diagnostics_cli -- --ignored --test-threads=1` |

### P1 — tracked, requires explicit acknowledgement (not auto-blocking)

| Item | Why not auto-P0 | Where recorded |
| --- | --- | --- |
| **Gate A real-NAT run** (`nat-probe`, two hosts on different networks, both directions, natural + `--relay-only`) | Requires two real hosts; cannot run in a single-machine script. Documented residual risk for a *developer preview*, not a blocker — but the maintainer must record its current status. | [Known MVP limitations](#known-mvp-limitations); [Sign-off](#sign-off) "Gate A status" field; `crates/spike-nat/results/results.md` |
| **Live-tail display gap** — streaming `room tail` renders only `message.text` (`agent.status`/`file.shared` rows appear only under `--offline`) | Cosmetic; the offline read is complete | [Known MVP limitations](#known-mvp-limitations) |
| **DX metric timings** (PRD §17.2) — first identity <1 min, two-peer room <3 min, first pipe <5 min | Human-timed, environment-dependent | [Demo verification](#demo-verification) (record measured values) |

## Pipe security review

Traceable to PRD §13.2. Confirm on the candidate build (proven by
`crates/iroh-rooms-net/tests/pipe_e2e.rs`, `crates/iroh-rooms-cli/tests/pipe_cli.rs`):

- [ ] `pipe expose` requires explicit `--allow <member-id>` (or equivalent); there is no default all-member exposure.
- [ ] The connector binds loopback (`127.0.0.1`) only; a non-loopback `--tcp` target is refused.
- [ ] The PRD §13.2.4 warning prints before forwarding and names **both** the exposed target and each allowed member.
- [ ] `pipe.closed` is emitted on clean exit (`owner_exit`); a `SIGKILL`'d pipe is later closeable by an owner/admin `pipe close`.
- [ ] The local audit vocabulary (`pipe.*` / `reject.*`) is present for open, connect, reject, and close.
- [ ] No terminal sharing is exposed (MVP scope).

## Blob verification review

Traceable to PRD §9.2. Confirm on the candidate build (proven by
`crates/iroh-rooms-net/tests/blob_e2e.rs`, `file_e2e.rs`,
`crates/iroh-rooms-cli/tests/file_cli.rs`,
`crates/iroh-rooms-core/tests/file_shared_hashes.rs`):

- [ ] BLAKE3-256 is recomputed on fetch and checked against the claimed hash.
- [ ] The two-gate ACL holds: the fetcher must be an active room member **and** the hash must be one the room actually referenced.
- [ ] A hash mismatch is a hard stop — no silent provider fallthrough.
- [ ] Saved file paths use a path-traversal-safe basename.
- [ ] The shipped size cap and the PRD §17.1 target agree, or the divergence is recorded (see [Known MVP limitations](#known-mvp-limitations)).

## Agent flow review

Traceable to PRD §13.3. Confirm on the candidate build (proven by
`crates/iroh-rooms-cli/tests/agent_cli.rs`, `agent_e2e.rs`,
`agent_invite_flow.rs`, `crates/iroh-rooms/tests/example_agent_e2e.rs`):

- [ ] An agent has its own identity and device key; its events are signed.
- [ ] An agent joins a room only through an explicit invite (no implicit room access).
- [ ] An agent joins at the least-privileged `agent` role.
- [ ] An agent cannot open a pipe unless separately authorized.
- [ ] Agent-produced artifacts (files, status) are content-addressed and verified the same way user artifacts are.
- [ ] A ticket presented by an uninvited key is rejected (identity-binding guard holds).

## Known MVP limitations

Drawn from `PRD.v0.3.md` §13.4/§14, the README status log, and the crate
`NOTES.md` files. A preview ships honest only if these are stated up front,
not discovered by a user:

- **★ Real-NAT connectivity measured across both environments (Gate A,
  2026-07-03/04).** S1 home-NAT ↔ cloud and S2 cellular CGNAT hotspot ↔ {cloud,
  home-NAT}: a direct hole-punched path establishes on every run in both
  environments, establishment + relay reachability pass both directions (incl.
  inbound to the CGNAT client), and the real carrier crossed both NATs. nat-probe
  labels runs `mixed` (relay stays Active as warm standby, even at `--settle 30`)
  — a classifier artifact, not a punch failure; corroborated on S1 by the
  issue-#43 SDK-daemon run. **Open (non-connectivity):** forced-relay throughput
  over the cellular uplink read 0.1–0.2 Mbit/s on small samples (below the
  ≥1 Mbit/s target — a larger-sample re-measure is owed), and the home-NAT→CGNAT
  reverse leg was not run. See
  [`crates/iroh-rooms-net/NOTES.md`](crates/iroh-rooms-net/NOTES.md) §"Gate A
  (real-network)" and [`crates/spike-nat/results/results.md`](crates/spike-nat/results/results.md).
  A 2026-07-07 local↔`demo1` refresh also passed both directions, natural and
  relay-only; relay-only measured 4.1 Mbit/s BtoA and 1.3 Mbit/s AtoB.
- **★ No cloud inbox; no guaranteed offline delivery.** Files and pipes
  require a provider to be online; messages deliver only when peers are
  online or reconnect (PRD §14).
- **No group E2EE, no PFS, no advanced key rotation, no secure multi-device
  recovery.** (PRD §13.4 items 1–4)
- **No native invite revocation; bounded leaked-ticket model.** A ticket is a
  scoped, key-bound capability until it expires or is consumed by departure
  rules (PRD §13.4 item 10, §13.5 item 1). Phase 2.5 Production Beta accepts
  this only under
  `docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`; Production
  GA still needs native revocation or an explicit re-acceptance of the narrow
  scope.
- **Unencrypted local storage.** `rooms.db` / `blobs/` are plaintext on disk
  (storage encryption is roadmap PRD §13.5 item 9). Phase 2.5 Production Beta
  accepts this only under the trusted-local-machine scope in
  `docs/decisions/ADR-0001-local-storage-posture.md`; Production GA still needs
  encryption or a narrower deployment claim.
- **Join-bootstrap privacy trade-off (Approach A).** With `--accept-joins`, a
  dialer who knows `room_id` + admin `EndpointId` is served the
  **secret-free** membership sub-DAG during the open-invite window; no
  capability secret is disclosed, and the window closes when the flag is off
  (IR-0104).
- **Live-tail display gap.** Streaming `room tail` renders only
  `message.text`; `agent.status`/`file.shared` rows show only in
  `room tail --offline`.
- **`SIGKILL` leaves a pipe open on the log** until an owner/admin
  `pipe close` (clean `SIGINT`/`SIGTERM` emit `pipe.closed{owner_exit}`)
  (IR-0108).
- **CLI installs no tracing subscriber.** Audit is via explicit CLI sinks:
  operator-facing stderr lines plus `<IROH_ROOMS_HOME>/audit.ndjson` for local
  persistence. Phase 2.5 Production Beta accepts this local audit posture in
  `docs/decisions/ADR-0003-persistent-audit-posture.md`; `Tracing*Audit` output
  is still dropped wherever no explicit CLI sink is installed.
- **Small-room, single-immutable-admin, TCP-only-pipe, loopback-only-bind**
  scope (PRD §7, §18.4).
- **File-size cap reconciled.** Code enforces `MAX_SHARED_FILE_BYTES` =
  **100 MiB** (`104_857_600` bytes,
  `crates/iroh-rooms-core/src/event/constants.rs`), checked at share, at
  `file.shared` validation, and at fetch buffering. The PRD success metrics
  (PRD §18.6, PRD.v0.3 §17.1.9) previously read **25 MB**, a target that was
  never enforced; they now document the shipped 100 MiB cap, so this is no
  longer a per-build divergence to flag. The 100 MiB cap is not free and the
  PRD records the measured cost: a 100 MiB fetch was measured at ~134.6 MB
  consumer RSS (the collector allocates the next power of two) and ~2.004x
  disk amplification (payload written to the out path *and* re-imported into
  the blob store, with no GC or delete path in non-test code). A
  `cargo test -p iroh-rooms-core` guard pins the constant so the docs and the
  code cannot silently drift apart again. Note the test-only
  `IROH_ROOMS_MAX_SHARE_BYTES` override (`crates/iroh-rooms-cli/src/file.rs`)
  is not a user-facing knob: it is absent from `--help` and the size error
  still names the effective cap.

## Security warnings

The checklist confirms the *shipping build actually prints/enforces* each of
these (most are already covered by tests; this is the human cross-check):

- **Pipe exposure warning** — `pipe expose` prints the PRD §13.2.4 warning
  naming the exposed target and **each** allowed member, before forwarding
  (proven by `pipe_cli.rs`).
- **Ticket = password-grade capability** — `room invite` output ends with the
  password-grade warning; a ticket/secret never appears in any error or audit
  line (proven by `invite_cli.rs`, error-taxonomy tests).
- **Loopback-only bind** — non-loopback `--tcp` targets are refused; the
  connector binds `127.0.0.1` only (proven by `pipe_cli.rs` / `pipe_e2e.rs`).
- **Agents are not implicitly trusted** — least-privileged `agent` role; no
  room access without an explicit key-bound invite (PRD §13.3; proven by
  `agent_invite_flow.rs`).
- **Local-storage plaintext disclaimer** — the preview must state storage is
  unencrypted; there is no code warning today, so this is a doc-level
  disclosure requirement carried by this checklist and the release-notes
  template.

## Dependency / churn review

Run once per candidate (guidance; no automation beyond the commands
themselves):

- `cargo tree --workspace --edges normal` — confirm **no new runtime
  dependency** slipped into the shipping crates (`iroh-rooms-core`,
  `iroh-rooms-net`, `iroh-rooms-cli`, `iroh-rooms`). Dev-only additions
  (e.g. `proptest` in `iroh-rooms-core`'s `[dev-dependencies]`) are fine.
- Confirm the **iroh pin is unchanged** (`1.0.1`) or, if bumped, that the "no
  `ConnectionType` watcher" workaround in `spike-nat`/diagnostics still holds
  (project memory "iroh 1.0.1 has no ConnectionType watcher").
- Confirm the **spike crates stay isolated**: `spike-blobs`, `spike-nat`,
  `spike-transport` remain `publish = false`, off the shipping dependency
  tree, and referenced only as throwaway harnesses.
- `cargo update --dry-run` (or a lockfile diff vs. the previous preview tag)
  — eyeball the churn; flag any transitive major bumps.
- Cross-reference [`PHASE-0-GO-NO-GO.md`](PHASE-0-GO-NO-GO.md) §5 "Pinned
  dependency observations" — confirm none of the recorded caveats regressed.

## Demo verification

Dry-run [`docs/getting-started.md`](docs/getting-started.md) against the
candidate build. The automated proxy for this is
`full_demo_e2e.rs`'s `full_demo_two_humans_one_agent` (part of the P0 gated
online tier above); this section records the **human** confirmation:

- [ ] A developer can complete the full demo (identity → room → invite/join →
      message → file share/fetch → live pipe → agent status) using only
      `docs/getting-started.md`, without maintainer help.
- [ ] Record the three PRD §17.2 timings on this build:
  - Time to create first identity: `<t1>` (target: under 1 minute)
  - Time to create and join first two-peer room: `<t2>` (target: under 3 minutes)
  - Time to expose and connect to first live pipe: `<t3>` (target: under 5 minutes after install)
- [ ] Demo verdict: `<PASS/FAIL>`

## Release notes template

```markdown
# Iroh Rooms — Developer Preview <VERSION> (<DATE>)

Status: DEVELOPER PREVIEW. Not for production. No security audit has been performed.

## What you can do
- <one-line per shipped capability: identity, room, invite/join, message, file share/fetch,
  live pipe, agent status, Rust SDK>

## Highlights since <PREV_VERSION>
- <notable changes>

## Known limitations (read before relying on this)
- Real-NAT connectivity (Gate A): <current status — e.g. measured 2026-07-03/04 across both environments (home-NAT + cellular CGNAT); direct hole-punch + relay reachability confirmed; a larger-sample cellular relay-throughput re-measure owed>.
- No cloud inbox / no guaranteed offline delivery; peers must be online.
- Local storage is unencrypted; no native invite revocation (ADR-0002 bounded
  model); no group E2EE / PFS.
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

## Sign-off

A preview is **READY** only when `scripts/release-readiness.sh` exits `0` —
that is the operational definition of "ready" this checklist uses (AC4). Paste
its output below; do not hand-wave this section.

| Field | Value |
| --- | --- |
| Verdict | `<READY / NOT READY>` |
| `release-readiness: …` line | `<paste the exact line scripts/release-readiness.sh printed>` |
| Gate A status | `<PENDING / measured — link crates/spike-nat/results/results.md>` |
| Reviewer | `<name>` |
| Date | `<YYYY-MM-DD>` |
