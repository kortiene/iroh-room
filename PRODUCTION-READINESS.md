# Production Readiness Plan

Status: Phase 2.5 proposal / execution plan. This document defines the work
required to move Iroh Rooms from Developer Preview to a production-grade
post-MVP release under the existing small-room scope.

This is not a feature-expansion plan. Desktop UX, calls, task events, advanced
agent workflows, multi-device identity, and the Phase 5 availability layer stay
out of scope until the current CLI/runtime can be trusted in real use.

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
7. A real beta cohort has validated the workflow on real machines and networks.

The recommended next phase is:

```text
Phase 2.5 - Production Release Candidate
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
| Production Beta | Production P0 gates green, security/ops docs complete, limited beta cohort running | Production sign-off plus beta plan |
| Production GA | Beta exit criteria met, release artifacts signed, support/rollback/security process active | Production sign-off plus beta results |

Do not use "production" for a build that has only passed the Developer Preview
gate. The preview gate proves the MVP works; the production gate proves the
project can be relied on within its stated constraints.

## Production P0 Gates

Every item in this section blocks a Production Beta label.

### P0.1 Preview Gate Is Green

`scripts/release-readiness.sh` must exit `0` on the release candidate.

Required evidence:

- Paste the exact `release-readiness: READY` line into the production sign-off.
- Record commit SHA, platform, rustc version, and date.
- Attach failures or skips; no skipped online tier is acceptable for production.

### P0.2 Real-Network Evidence Is Current

Gate A evidence must be refreshed or explicitly accepted if still recent.

Required evidence:

- `crates/spike-nat/results/results.md` contains measured two-host runs, not a
  placeholder.
- The production sign-off states whether the July 2026 Gate A evidence,
  including the 2026-07-07 local↔`demo1` refresh, is being accepted as current
  or replaced by a new run.
- The remaining cellular forced-relay throughput caveat is either re-measured
  with a larger transfer or explicitly accepted as non-blocking for the scoped
  release.
- The missing home-NAT to CGNAT reverse leg is either run or explicitly accepted
  as non-blocking with rationale.

### P0.3 Threat Model And Security Review

The security model must be documented before a production label.

Required evidence:

- A threat model exists at `docs/security/threat-model.md`.
- It covers identities, device keys, invite tickets, room membership, blobs,
  pipes, local storage, relay metadata, malicious peers, compromised devices,
  stolen tickets, and removed members.
- All production-blocking threats have owner, mitigation, and test evidence.
- An external or independent security review is completed or the release label
  remains Production Beta with that caveat clearly stated.

### P0.4 Invite And Access Revocation Story

Production cannot rely only on "do not leak tickets" language.

Required evidence:

- Ticket expiry behavior is tested and documented.
- The release has either native invite revocation or a documented bounded-risk
  model that limits leaked-ticket blast radius.
- For Phase 2.5 Production Beta, the bounded-risk model is accepted in
  `docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md`.
- Removed members lose blob and pipe access consistently.
- Active pipe sessions are torn down or fail closed after removal is learned.
- Tests cover wrong identity, expired ticket, removed member, non-member, and
  uninvited agent paths.

### P0.5 Local Data Handling

Plaintext local data may be acceptable only if the production claim says so
clearly. It is not acceptable for users to discover this by reading source.
For Phase 2.5, the accepted beta posture is captured in
`docs/decisions/ADR-0001-local-storage-posture.md`: trusted local machines,
plaintext room data disclosed, no local-compromise-resistance claim.

Required evidence:

- Storage encryption is implemented or deliberately deferred in
  `docs/decisions/ADR-0001-local-storage-posture.md`.
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

These do not block Production Beta if explicitly accepted, but they should block
Production GA unless waived with rationale.

| Gate | Required outcome |
| --- | --- |
| Multi-platform CI | Linux and macOS run the deterministic gate; platform-specific caveats documented |
| Packaging | At least one supported binary install path is documented and verified |
| Human DX timings | Identity, two-peer room, and first pipe timing targets are measured on a release candidate |
| Beta cohort | 5-10 technical users complete the demo on real machines |
| Product validation | Users can explain the availability model and identify Live Pipe value |
| Dependency review | Runtime dependency churn is reviewed before release |
| Support loop | Issues template and triage labels exist for security, networking, data loss, and UX failures |

## Production P2 Gates

These improve GA quality but should not distract from P0/P1.

- `room list` and consistent JSON output across read commands.
- Live `room tail` renders `agent.status` and `file.shared`, not only
  `message.text`.
- A guided local smoke-check command for new users.
- Optional signed release artifacts.
- Optional cargo/crates.io publication of the stable SDK tier.
- More user-friendly network diagnostics.

## Workstreams

### Security

Owner profile: protocol/security-minded maintainer.

Deliverables:

1. Threat model.
2. Invite revocation or bounded leaked-ticket decision.
3. Persistent audit implementation and retention/privacy decision.
4. Storage encryption decision.
5. Independent review checklist.

Acceptance criteria:

- Every security limitation in `RELEASE-READINESS.md` is either fixed, scoped,
  or explicitly carried into the production label.
- Secret-leakage tests remain green.
- Removed-member access loss is proven for events, blobs, and pipes.

### Reliability And Networking

Owner profile: networking/runtime maintainer.

Deliverables:

1. Gate A refresh plan.
2. Relay throughput re-measure.
3. Missing reverse-leg run or waiver.
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

### Product Beta

Owner profile: product/field maintainer.

Deliverables:

1. Beta cohort plan.
2. Demo script and observation worksheet.
3. DX metric collection.
4. Product validation memo.

Acceptance criteria:

- At least 5 external or semi-external technical users complete the core demo.
- At least 80% can explain the online-peer availability model.
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

- `scripts/release-readiness.sh` exits `0`.
- `scripts/production-readiness.sh` automated preflight passes.
- Manual P0 sign-offs are complete.
- Release notes and install docs are complete.

### Milestone D - Production Beta

Exit criteria:

- Beta cohort starts with scoped expectations.
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
| PR-0003 | Accept scoped plaintext beta via ADR-0001 | P0 | security/data | Trusted-local-machine beta; GA still needs encryption or narrower scope |
| PR-0004 | Define invite revocation or bounded leaked-ticket model | P0 | security/protocol | ADR-0002 accepts bounded-risk model for Production Beta; GA still needs native revocation or explicit re-acceptance |
| PR-0005 | Sign off persistent audit retention/privacy posture | P0 | observability | ADR-0003 accepts local `audit.ndjson` posture for Production Beta; GA should revisit retention/rotation/tamper evidence |
| PR-0006 | Add schema migration and compatibility policy | P0 | data/protocol | Core v1 fixture gate implemented; next candidate must preserve previous-candidate data evidence |
| PR-0007 | Add backup, restore, and export docs | P0 | operations | Avoid data-loss ambiguity |
| PR-0008 | Refresh Gate A relay-throughput evidence | P0 | networking | 2026-07-07 local↔`demo1` refresh added; cellular relay larger-transfer caveat and home-NAT→CGNAT reverse-leg waiver/run still need sign-off |
| PR-0009 | Add production release sign-off template | P0 | release | Implemented in `docs/operations/release-operations.md` |
| PR-0010 | Add privacy-preserving bug report template | P1 | support | Needed for beta |
| PR-0011 | Add macOS/Linux release artifact checklist | P1 | release | Implemented via release artifact script and install/rollback runbook |
| PR-0012 | Measure human DX timings on a release candidate | P1 | product | PRD section 17.2 |
| PR-0013 | Run 5-10 user production beta | P1 | product | Real machines, real networks |
| PR-0014 | Add `room list` | P2 | CLI | High UX value, not a production blocker |
| PR-0015 | Render all MVP event types in live `room tail` | P2 | CLI | Current gap is disclosed |

## Final Recommendation

Proceed with Phase 2.5 before starting Phase 3. The project has enough runtime
surface to create real user value. The highest-leverage work is now reducing
security, data, release, and operational ambiguity around that surface.

The production claim should remain deliberately narrow until beta evidence says
otherwise.
