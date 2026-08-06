# Production Readiness Plan

Status: live evidence and gate plan, governed by [`ROADMAP.md`](ROADMAP.md)
RB-1. This document defines production-readiness work and acceptance evidence;
RB-1 defines roadmap scope, ordering, and the cohort boundary.

The earlier Phase 2.5 sequence treated this as a narrow pre-Phase-3 plan. The
maintainer decision recorded by ADR-0012 supersedes that ordering. The cohort
waits until every cohort-blocking `PRE` row—including desktop, calls,
task/agent work, availability, v2, and Cockpit—is complete, every
cohort-blocking `COND` row is explicitly resolved, and every cohort-blocking
`FC` gate passes on the exact candidate. Historical release sign-offs and
waivers remain evidence for their releases, but do not complete an RB-1 row.

## Executive Summary

Iroh Rooms is post-MVP in the narrow product sense: the CLI MVP workflow exists
and the repository has moved into Developer Preview. Production-grade post-MVP
requires a different bar:

1. The release gate is repeatable and green on every candidate build.
2. The security model is explicit, reviewed, and reflected in defaults.
3. Access loss, invite expiry/revocation, pipe teardown, and blob authorization
   are observable and testable.
4. Data handling is honest and recoverable: local storage, backups, export,
   schema migration, and upgrade/rollback behavior are defined.
5. Networking limitations are measured, user-visible, and diagnosable.
6. SDK/protocol compatibility is governed by versioned contracts.
7. A final external cohort validates the roadmap-complete candidate on real
   machines and networks after all pre-cohort work and candidate gates finish.

The next production-readiness stage is:

```text
RB-1 - Roadmap Completion and Candidate Evidence
```

The goal is a scoped production claim:

> Iroh Rooms supports small private CLI-first rooms for technical users who
> understand the online-peer availability model, with signed events, verified
> artifacts, authenticated TCP live pipes, explicit agent participation, and no
> central application server.

The release must not claim enterprise readiness, strong anonymity, guaranteed
offline delivery, large public-room support, secure multi-device recovery,
public app UX, or audited end-to-end group encryption.

## Release Labels

Use these labels consistently in docs, release notes, and issue milestones.

| Label | Meaning | Required evidence |
| --- | --- | --- |
| Developer Preview | MVP workflow works, known limitations disclosed, P0 preview gate green | `scripts/release-readiness.sh` exits `0` |
| Roadmap-complete cohort candidate | Every cohort-blocking RB-1 PRE/COND row complete and every FC gate green | Exact tagged candidate plus RB-1 evidence map |
| Production Beta | Historical controlled-beta label, or the roadmap-complete candidate while milestone #8 is active | Versioned sign-off plus exact cohort plan |
| Production GA | Beta exit criteria met, release artifacts signed, support/rollback/security process active | Production sign-off plus beta results |

Do not use "production" for a build that has only passed the Developer Preview
gate. The preview gate proves the MVP works; the production gate proves the
project can be relied on within its stated constraints.

## Production P0 Gates

Every item in this section blocks the roadmap-complete cohort candidate and
Production GA. Earlier versioned Production Beta sign-offs remain historical
records; their waivers do not satisfy RB-1.

### P0.1 Preview Gate Is Green

`scripts/release-readiness.sh` must exit `0` on the release candidate.

Required evidence:

- Paste the exact `release-readiness: READY` line into the production sign-off.
- Record commit SHA, platform, rustc version, and date.
- Attach failures or skips; no skipped online tier is acceptable for production.

### P0.2 Real-Network Evidence Is Current

Gate A evidence must be refreshed for the roadmap-complete candidate whenever
transport behavior or the evidence environment has changed.

Required evidence:

- `crates/spike-nat/results/results.md` contains measured two-host runs, not a
  placeholder.
- The production sign-off identifies which historical Gate A evidence remains
  applicable and links the required final-candidate refresh.
- The cellular forced-relay throughput caveat is re-measured with a
  representative larger transfer.
- The missing home-NAT to CGNAT reverse leg is run.
- Any unavailable hardware is acquired or rented. Changing the required
  evidence needs a versioned RB-1 amendment; a permanent waiver is not
  completion.

### P0.3 Threat Model And Security Review

The security model must be documented before a production label.

Required evidence:

- A threat model exists at `docs/security/threat-model.md`.
- It covers identities, device keys, invite tickets, room membership, blobs,
  pipes, local storage, relay metadata, malicious peers, compromised devices,
  stolen tickets, and removed members.
- All production-blocking threats have owner, mitigation, and test evidence.
- An independent human reviewer or firm covers every shipped trust surface and
  completes a final-candidate delta review. The existing non-author-agent v2
  audit covers governance/fork handling only.

### P0.4 Invite And Access Revocation Story

Production cannot rely only on "do not leak tickets" language.

Required evidence:

- Ticket expiry behavior is tested and documented.
- Native invite revocation is implemented and reachable through the shipped
  product surfaces.
- For Phase 2.5 Production Beta, the bounded-risk model is accepted in
  `docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`.
  That remains historical beta evidence, not RB-1 completion.
- Removed members lose blob and pipe access consistently.
- Active pipe sessions are torn down or fail closed after removal is learned.
- Tests cover wrong identity, expired ticket, removed member, non-member, and
  uninvited agent paths.

### P0.5 Local Data Handling

Plaintext local data was accepted only for the historical scoped beta posture.
It is not sufficient for the roadmap-complete cohort candidate. The earlier
posture is captured in
`docs/decisions/ADR-0001-local-storage-posture.md`: trusted local machines,
plaintext room data disclosed, no local-compromise-resistance claim.

Required evidence:

- Database, key, blob, audit, and recovery storage encryption is implemented as
  assigned by RB-1.
- Backup and restore instructions exist.
- Export instructions exist, even if the initial export is low-level.
- SQLite schema migration policy exists.
- Upgrade and rollback behavior is documented.
- Secrets are never included in backup/export examples.

### P0.6 Persistent Audit And Diagnostics

Stderr-only audit is not sufficient for production incident reconstruction. The
CLI now writes security and lifecycle callbacks to `<IROH_ROOMS_HOME>/audit.ndjson`
through the explicit local audit sink; Phase 2.5 Production Beta accepts the
local retention/privacy posture in
`docs/decisions/ADR-0003-persistent-audit-posture.md`.

Required evidence:

- Pipe open/connect/reject/close events have a persistent local audit sink.
- Blob authorization rejects and peer admission rejects are observable.
- Diagnostics expose enough information to distinguish direct, relay, offline,
  unauthorized, unavailable, and local misconfiguration states without leaking
  secrets.
- Audit retention and privacy implications are documented.

### P0.7 Compatibility And Migration

Production requires version-to-version continuity.

Required evidence:

- Stable protocol/event compatibility fixtures exist.
- A room created by version `N` remains readable by version `N+1`.
- SQLite migrations are additive or explicitly destructive with a migration
  warning.
- SDK stable vs experimental API policy is documented.
- Published artifacts, examples, and docs agree on feature gates.

Current Beta evidence:

- Versioned v1 wire fixtures live in
  `crates/iroh-rooms-core/tests/fixtures/v1/events.txt`.
- The v1 SQLite schema fixture lives in
  `crates/iroh-rooms-core/tests/fixtures/v1/store_v1_schema.sql`.
- `cargo test -p iroh-rooms-core --features store --test compatibility`
  decodes the v1 bytes, verifies event IDs/signatures, folds the membership
  snapshot, imports the events into the current store, and migrates a v1
  SQLite fixture to the current schema while preserving authoritative event
  bytes.
- First Production Beta has no prior production-candidate binary database to
  preserve. Starting with the next candidate, the release owner must add or
  refresh a previous-candidate data fixture before claiming upgrade continuity.

### P0.8 Release Operations

The release must be installable, repeatable, and reversible.

Required evidence:

- Build artifacts are versioned and checksummed.
- Release notes include capabilities, limitations, security notes, and exact
  gate output.
- Install and uninstall instructions exist for supported platforms.
- Rollback instructions exist.
- A privacy-preserving bug report template exists.
- The release owner signs off on the final checklist.

Current Beta evidence:

- `scripts/build-release-artifacts.sh` builds a versioned
  `iroh-rooms` archive from a recorded commit and emits a `.sha256` file.
- `docs/operations/install-uninstall.md` defines source install, binary
  install, checksum verification, local data configuration, uninstall, data
  removal, backup, and rollback.
- `docs/operations/production-beta-release-notes-template.md` provides the
  release notes template with limitations before install/run instructions.
- `docs/operations/release-operations.md` contains the production sign-off
  template and stop-ship rules.
- `.github/ISSUE_TEMPLATE/bug_report.md` provides privacy-preserving bug report
  guidance.

## Production P1 Gates

RB-1 classifies these outcomes individually. Included implementation and
release rows block the cohort candidate; human/product evidence remains a
cohort-dependent GA requirement. A waiver does not complete an included row.

| Gate | Required outcome |
| --- | --- |
| Multi-platform CI | Linux and macOS run the deterministic gate; platform-specific caveats documented |
| Packaging | Every supported artifact is documented and verified on native hardware |
| Human DX timings | Identity, two-peer room, and first pipe timing targets are measured on a release candidate |
| Final cohort | Three named participants run the 30-day milestone #8 loop after the roadmap-complete candidate is ready |
| Product validation | Users can explain the availability model and identify Live Pipe value |
| Dependency review | Runtime dependency churn is reviewed before release |
| Support loop | Issues template and triage labels exist for security, networking, data loss, and UX failures |

## Production P2 Gates

These were previously described as optional GA quality work. RB-1 now includes
the explicitly named deliverables before the cohort unless a versioned
amendment changes them.

- `room list` and consistent JSON output across read commands.
- Live `room tail` renders `agent.status` and `file.shared`, not only
  `message.text`.
- A guided local smoke-check command for new users.
- Signed release artifacts and verifiable provenance.
- Cargo/crates.io publication of the supported stable SDK tier.
- More user-friendly network diagnostics.

## Workstreams

### Security

Owner profile: protocol/security-minded maintainer.

Deliverables:

1. Threat model.
2. Native invite revocation.
3. Persistent, retained, rotated, tamper-evident audit.
4. Local database, key, blob, audit, and recovery-storage encryption.
5. Independent full-surface review and final-candidate delta.

Acceptance criteria:

- Every security limitation assigned to an RB-1 implementation row is fixed and
  evidenced before cohort activation.
- Secret-leakage tests remain green.
- Removed-member access loss is proven for events, blobs, and pipes.

### Reliability And Networking

Owner profile: networking/runtime maintainer.

Deliverables:

1. Gate A refresh plan.
2. Relay throughput re-measure.
3. Missing reverse-leg run.
4. Diagnostics improvements.
5. Failure-mode runbook.

Acceptance criteria:

- Production release notes include current real-network evidence.
- Users can tell whether a failure is offline peer, unauthorized peer,
  unavailable blob, relay/direct path issue, or local config error.

### Compatibility And Data

Owner profile: storage/protocol maintainer.

Deliverables:

1. Schema migration policy.
2. Backward compatibility fixtures.
3. Backup/restore/export docs.
4. Upgrade/rollback runbook.

Acceptance criteria:

- A release candidate can read the v1 compatibility fixture and, after the
  first Production Beta, a room created by the previous production candidate.
- A failed migration does not silently corrupt the authoritative event log.

### Release Operations

Owner profile: release owner.

Deliverables:

1. Production sign-off template.
2. Build artifact checklist.
3. Install/uninstall docs.
4. Bug report template.
5. Release notes template for Production Beta and GA.

Acceptance criteria:

- A new maintainer can cut a candidate using docs and scripts only.
- The release notes truthfully separate supported capabilities from known
  limitations.

### Final External Cohort

Owner profile: product/field maintainer.

Deliverables:

1. Cohort activation plan bound to the exact roadmap-complete candidate.
2. Demo script and observation worksheet.
3. DX metric collection.
4. Product validation memo.

Acceptance criteria:

- Three named external participants complete the recorded attempts during the
  30-day window.
- Comprehension of the online-peer availability model is recorded for every
  participant.
- At least one real workflow validates Live Pipe over a public tunnel.

## Phase 2.5 Milestones

### Milestone A - Production Gate Skeleton

Exit criteria:

- `PRODUCTION-READINESS.md` exists.
- `scripts/production-readiness.sh` exists as an automated preflight.
- README and CONTRIBUTING link to the production readiness plan.
- The script does not claim final production readiness; it only checks
  automatable evidence, fails on missing production P0 artifacts, and points to
  manual sign-offs.

### Milestone B - Security And Data Decisions

Exit criteria:

- Threat model drafted and reviewed.
- Storage encryption decision made.
- Invite revocation/bounded-risk decision made via ADR-0002.
- Persistent audit retention/privacy decision made via ADR-0003.
- Backup/export/restore docs drafted.

### Milestone C - Release Candidate

Exit criteria:

- Every cohort-blocking RB-1 `PRE`/`COND` row is complete.
- `scripts/release-readiness.sh` exits `0`.
- `scripts/production-readiness.sh` automated preflight passes.
- Manual P0 sign-offs are complete.
- Release notes and install docs are complete.
- Every recurring RB-1 `FC` gate is bound to the exact tagged candidate.

### Milestone D - Production Beta

Exit criteria:

- Three named participants are confirmed and milestone #8 is assigned its
  30-day due date against the exact roadmap-complete candidate.
- The final external cohort starts with scoped expectations only after
  Milestone C and every RB-1 candidate gate are complete.
- Issues are triaged daily during the beta window.
- No P0 security/data-loss/networking blockers remain open.

### Milestone E - Production GA Decision

Exit criteria:

- Beta results are documented.
- Open risks are either resolved or accepted in writing.
- The project has a dated Production GA decision memo.

## Initial Backlog

Use these as issue seeds. Keep each issue small enough to review independently.

| ID | Title | Priority | Area | Notes |
| --- | --- | --- | --- | --- |
| PR-0001 | Add production readiness plan and preflight script | P0 | release | This document and script |
| PR-0002 | Draft security threat model | P0 | security | Required before Production Beta |
| PR-0003 | Accept scoped plaintext beta via ADR-0001 | P0 | security/data | Historical beta evidence; RB-1 requires the included encryption rows before cohort activation |
| PR-0004 | Define invite revocation or bounded leaked-ticket model | P0 | security/protocol | ADR-0002 is historical beta evidence; RB-1 requires native invite revocation before cohort activation |
| PR-0005 | Sign off persistent audit retention/privacy posture | P0 | observability | ADR-0003 accepts local `audit.ndjson` posture for Production Beta; GA should revisit retention/rotation/tamper evidence |
| PR-0006 | Add schema migration and compatibility policy | P0 | data/protocol | Core v1 fixture gate implemented; next candidate must preserve previous-candidate data evidence |
| PR-0007 | Add backup, restore, and export docs | P0 | operations | Avoid data-loss ambiguity |
| PR-0008 | Refresh Gate A relay-throughput evidence | P0 | networking | 2026-07-07 local↔`demo1` refresh added; cellular relay larger-transfer and home-NAT→CGNAT reverse-leg runs remain owed by RB-NET-007/008; the 2026-07-14 `relay-only-test` seam is tooling only, not completion |
| PR-0009 | Add production release sign-off template | P0 | release | Implemented in `docs/operations/release-operations.md` |
| PR-0010 | Add privacy-preserving bug report template | P1 | support | Needed for beta |
| PR-0011 | Add macOS/Linux release artifact checklist | P1 | release | Implemented via release artifact script and install/rollback runbook |
| PR-0012 | Measure human DX timings on a release candidate | P1 | product | PRD section 17.2 |
| PR-0013 | Run the final external cohort | P1 | product | Three named participants, 30 days, exact roadmap-complete candidate; cohort-dependent |
| PR-0014 | Add `room list` | P2 | CLI | Now included as cohort-blocking RB-UP-101 |
| PR-0015 | Render all MVP event types in live `room tail` | P2 | CLI | Now included as cohort-blocking RB-UP-102/103 |

## Final Recommendation

Follow [`ROADMAP.md`](ROADMAP.md): freeze the baseline, record #162 Trigger 3,
materialize dependency-ordered issues, make D-9 the first technical decision,
complete every cohort-blocking `PRE` row, and explicitly resolve every
cohort-blocking `COND` row before cutting the roadmap-complete cohort candidate.
Then pass every cohort-blocking `FC` gate on that exact candidate, confirm three
named participants, and start the 30-day final cohort.

The production claim remains evidence-bound. Cohort-discovered fixes may require
a replacement candidate, affected gate reruns, and a restarted or extended
cohort window before GA.
