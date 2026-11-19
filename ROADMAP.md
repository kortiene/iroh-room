# Iroh Rooms Roadmap Baseline — RB-1

| Field | Value |
| --- | --- |
| Baseline | `RB-1.0` |
| Scope date | 2026-08-06 |
| Status | Active; accepted by #228 and merged on 2026-08-07 |
| Governing decision | [`ADR-0012`](docs/decisions/ADR-0012-roadmap-completion-before-external-cohort.md) |
| Cohort rule | Every cohort-blocking `PRE`, `COND`, and `FC` row must be complete before the external cohort starts |
| Amendment rule | Scope changes require a reviewed, versioned RB-1.x amendment |
| Active amendment | `RB-1.1` — v0.1 GA scope (SDK + CLI), [`ADR-0014`](docs/decisions/ADR-0014-v0.1-ga-sdk-cli-small-rooms-scope.md); proposed until merged |

RB-1 is the finite, auditable contract for completing the Iroh Rooms roadmap
before the final external builder cohort. It compiles the repository's
previously overlapping product, protocol, security, operations, platform,
release, and extension plans into one authority without rewriting their
historical records.

The cohort is deliberately not treated as pre-roadmap evidence. Work that can
only be produced by cohort participants—participant attempts, product evidence,
cohort-discovered fixes, the Discussions decision, and the final GA decision—is
classified separately below.

## Authority and source order

When sources disagree, use this order:

1. This active baseline and its accepted RB-1.x amendments.
2. Accepted decision records that refine a row without changing its inclusion.
3. The current product requirements, accepted architecture, production plans,
   and accepted implementation specifications cited by the row.
4. GitHub issues as execution records and evidence links.
5. Historical requirements, audits, release notes, and sign-offs as evidence
   only.

[`PRD.v0.3.md`](PRD.v0.3.md) is the current product requirements document.
[`PRD.md`](PRD.md) is the historical v0.2 record and does not compete with
v0.3. A v0.2-only idea is not added merely because it remains in that file; it
must be reaffirmed by a current source or an explicit baseline amendment.

A specification `Status:` field, issue closure, milestone closure, waiver, or
`closed:not_planned` disposition cannot change roadmap scope. Approved release
records remain immutable evidence; corrections are additive errata.

## Classification and status vocabulary

| Code | Meaning |
| --- | --- |
| `PRE` | Required implementation, documentation, security, operations, platform, or release work before cohort activation. |
| `COND` | A condition or product choice that must be explicitly resolved before the cohort. A negative outcome cannot silently remove another included row. |
| `FC` | A recurring gate that must pass on the exact roadmap-complete cohort candidate and again after any material candidate change. |
| `COHORT` | Work that necessarily occurs at cohort activation, during the cohort, or afterward. |
| `NON-GOAL` | An explicit boundary accepted by RB-1. Adding it requires an amendment. |

Statuses are `proposed`, `not-started`, `partial`, `blocked`, `complete`, and
`deferred`. `Complete` is permitted only when all completion rules below hold.

## Completion and amendment rules

A required row is complete only when:

1. it has a GitHub issue in the owning repository;
2. every dependency is complete;
3. its implementation and documentation are merged;
4. its acceptance tests and evidence artifacts are linked from the issue;
5. every required hardware operator or independent reviewer has supplied the
   named evidence; and
6. the issue is closed as completed.

Waivers can qualify a historical release, but do not complete an RB-1 row. If a
maintainer decides that a deliverable is no longer required, the change must be
an RB-1.x amendment that names the affected IDs, explains the reason, updates
dependencies and cohort flags, and links the approval. Rows may be added only
the same way.

`RB-GOV-004` materialized every required row into issues after baseline
approval. The owner column records the live issue pointers; issue bodies retain
the exact dependency expressions and expanded links for every finite expression.

The six completion rules continue to apply to every execution row and to
`RB-GOV-004`. The one-time activation rows are narrow exceptions:
`RB-GOV-001` and `RB-GOV-002` completed through merged PR #228;
`RB-GOV-003` completed through the dated [`FIRED` record](https://github.com/kortiene/iroh-room/issues/162#issuecomment-5223556761)
and [additive clarification](https://github.com/kortiene/iroh-room/issues/162#issuecomment-5223576552)
on #162. `RB-GOV-003` deliberately leaves #162 open because its exact
deliverable requires that continuing trigger tracker to remain open. No other
row inherits this exception.

## Preservation invariants

All implementation and verification work must preserve these constraints:

- Frozen v1 and v2 signed bytes, legacy domains, candidate records,
  checkpoints, fork envelopes, compatibility fixtures, and approved release
  records are never rewritten. Corrections are additive and versioned.
- `admin_seq` remains complete carrier-chain DAG depth. It is never scoped to
  membership events. The T18 withheld-removal detector and conformance Vector
  18 remain intact.
- Any divergence-result cache is keyed only by
  `membership_projection_generation`.
- `docs/protocol.md` continues to embed the protocol source through its
  `include_str!` drift gate.
- v2 uses a new ALPN/schema. Live v1 history is not reinterpreted as v2.

## Source register

| Area | Sources of scope and evidence |
| --- | --- |
| Product | [`PRD.v0.3.md`](PRD.v0.3.md), [`README.md`](README.md), [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| Historical product record | [`PRD.md`](PRD.md), treated as v0.2 evidence only |
| Production and security | [`PRODUCTION-READINESS.md`](PRODUCTION-READINESS.md), [`RELEASE-READINESS.md`](RELEASE-READINESS.md), [`SECURITY.md`](SECURITY.md), [`docs/security/threat-model.md`](docs/security/threat-model.md), ADR-0001 through ADR-0003 |
| Operations and compatibility | [`docs/operations/`](docs/operations/), [`docs/compatibility.md`](docs/compatibility.md), [`docs/sdk-coverage.md`](docs/sdk-coverage.md), release workflows and scripts |
| V2 architecture | GitHub #134, #162, [`ADR-0004`](docs/post-ga/decisions/ADR-0004-accept-v2-large-community-architecture.md), ADR-0007 through ADR-0011, `docs/post-ga/specs/v2-*`, and the frozen v2 vectors |
| Admin-fork safety | ADR-0005, [`ADR-0006`](docs/decisions/ADR-0006-fold-level-divergence-detector.md), T18 and Vector 18 conformance evidence |
| Network and scale | Gate A specifications/results, gossip/N=40 specifications/results, GitHub #192/#214, [`docs/post-ga/specs/measure-real-network-gossip-overlay.md`](docs/post-ga/specs/measure-real-network-gossip-overlay.md) |
| Product extensions | [`docs/cockpit-backlog.md`](docs/cockpit-backlog.md), [`specs/room-list-read-cli.md`](specs/room-list-read-cli.md), [`specs/live-tail-ndjson-stream.md`](specs/live-tail-ndjson-stream.md) |
| Community and cohort | [`COMMUNITY.md`](COMMUNITY.md), [`docs/community/first-cohort.md`](docs/community/first-cohort.md), GitHub #99 through #103, milestones #5 and #8 |
| Evidence only | Historical audits, release notes/sign-offs, closed milestones, GitHub review threads, workflow runs, and platform/network result corpora |

## RB-1.1 — v0.1 GA scope

[`ADR-0014`](docs/decisions/ADR-0014-v0.1-ga-sdk-cli-small-rooms-scope.md)
amends RB-1.0: **v0.1 GA is the Rust SDK (`iroh-rooms` crate) and the
`iroh-rooms` CLI built on it, for small private rooms (the 5-member full-mesh
cap)**. The SDK is published with its dependency closure (`iroh-rooms-core`,
`iroh-rooms-crypto`, and `iroh-rooms-net` for the `experimental` tier); the CLI
ships as signed binaries on the four platform triples that already ship. The
classes below sit alongside `PRE`/`COND`/`FC`/`COHORT`/`NON-GOAL`; they decide
what blocks the v0.1 GA candidate, not whether a row stays in the roadmap.

| Code | Meaning |
| --- | --- |
| `GA-P0` | Must be complete, or explicitly dispositioned where the row says so, before the v0.1 GA candidate is tagged. |
| `GA-P1` | Targeted for v0.1 GA. If incomplete at the candidate, recorded as a dated, named limitation in the GA release notes. |
| `POST-GA` | Stays in RB-1 with its row, dependencies, and evidence unchanged; no longer blocks the v0.1 GA candidate or its cohort. Cohort-blocking column reads `No (POST-GA, RB-1.1)`. |

For the v0.1 GA candidate, dependency expressions that name `POST-GA` or
unclassified rows (for example `RB-SDK-001`'s list, or "all PRE/COND rows" in
the `FC` gates) read as "the `GA-P0`/`GA-P1` rows that the expression names".
Rows not named in this section keep their RB-1.0 classification and are
**unclassified pending maintainer decision**; they do not block the v0.1 GA
candidate unless a later RB-1.x amendment classifies them.

### New row

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-GA-001 | Keep `verify`, `msrv`, and `release-readiness` green on a pinned stable toolchain, and re-enable the scheduled `release-readiness` workflow. | PRE / GA-P0 | ADR-0014; 2026-11-14 production-readiness audit | `iroh-room` / CI-fix branch `ci/pin-toolchain-clippy-1.100` (issue to be materialized) | — | Pinned toolchain in workflows, green runs on `main`, scheduled workflow active | Maintainer (workflow re-enable is a repository setting) | partial | Yes |
| RB-GA-002 | Ship CLI member removal: `room remove <ROOM_ID> <MEMBER_ID>` (admin only) and `room leave <ROOM_ID>`, with docs that say removed members lose access to new messages, files and pipes once peers see the removal and keep what they already had. | GA-P0 | RB-1.4; #343 reachability inventory; threat model T07/T09/T18 | `iroh-room` / #379 | — | CLI commands with CI-tier and online tests; README, getting-started, threat-model, and self-review updates | — | partial | Yes |

### v0.1 GA P0 gate

| Gate item | Rows | Issues | Note |
| --- | --- | --- | --- |
| CI green on a pinned toolchain | RB-GA-001 | CI-fix branch | — |
| CLI member removal | RB-GA-002 | #379 | **RB-1.4:** `room remove` / `room leave`; removal is enforced by peers once they see it, with no key rotation (OD-1) |
| Dependency remediation and supply-chain gate | RB-REL-003 | #238 | RustSec advisories resolved or dispositioned; `cargo-deny` and automated updates in CI |
| Signed releases and provenance | RB-REL-001, RB-REL-002 | #237, #248 | Covers CLI archives; the SDK crates' provenance is the crates.io publication record plus the tagged source commit |
| SDK publication | RB-SDK-002, RB-SDK-001 | #313, #306 | Publish `iroh-rooms` and its closure with versioning, ownership, and a release procedure; `RB-SDK-001` coverage reconciliation is scoped to the GA surface |
| Schema-evolution decision | RB-DEC-001 | #232 | ADR-0013 (#360); fixes the compatibility contract the published SDK carries |
| Encryption decision | RB-REL-007 | #343 | **OD-1 decided 2026-11-15 (RB-1.2):** v0.1 GA ships plaintext room content, encrypted in transit only, disclosed. `RB-SEC-011` (#268) and `RB-SEC-002` (#250) are `POST-GA`; `RB-REL-007` must add an encryption-claims entry |
| Independent security review | — | #344, #353 (`POST-GA`) | **OD-2 decided 2026-11-15 (RB-1.3):** no independent review for v0.1 GA; WAIVER-PBETA-0003 accepted for v0.1 GA only and disclosed, with an internal structured self-review ([`docs/security/v0.1-self-review.md`](docs/security/v0.1-self-review.md)) as the gate evidence. `RB-SEC-010` and `RB-FC-009` are `POST-GA` |
| Install/upgrade/rollback/uninstall on four platforms | RB-REL-004, RB-REL-005, RB-FC-004 | #265, #293, #347 | `RB-REL-005`'s dependency on RB-SEC-006 reads as the GA-P1 backup/restore state |
| Real-network evidence | RB-NET-007, RB-NET-008, RB-FC-005 | #235, #236, #348 | Measured legs, or a dated maintainer acceptance of WAIVER-PBETA-0001/0002 for v0.1 GA only. `RB-FC-005` is scoped to Gate A |
| Compatibility gate | RB-FC-006 | #349 | Scoped to v1, immutable-vector, prior-candidate (rc.5 data), and rollback compatibility; v2 migration is `POST-GA` |
| Candidate gates | RB-FC-001, -002, -003, -010, -011, -012 | #345, #346, #352, #354, #355, #356 | Scoped to the v0.1 GA candidate |

### v0.1 GA P1 targets

| Area | Rows | Issues |
| --- | --- | --- |
| Access and audit | RB-SEC-001, RB-SEC-016, RB-SEC-017, RB-SEC-018 | #249, #239, #254, #269 |
| Data | RB-SEC-006, RB-FC-008 (scoped to backup/restore of the GA storage posture), RB-SEC-004, RB-SEC-009 | #285, #351, #266, #294 |
| Shipped admin-fork detector (ADR-0006) | RB-ADM-001, RB-ADM-002, RB-ADM-003, RB-ADM-004 | #230, #240, #260, #241 |
| SDK/CLI surfaces | RB-UP-101..106, RB-CLI-001 | #255, #256, #257, #258, #259, #270, #261 |
| Operations | RB-REL-006 | #299 |

`RB-SEC-006` reads its `RB-SEC-004`/`RB-SEC-005` dependencies as "the GA
storage posture". `RB-SEC-004` depends on the unclassified `RB-SEC-003`; if that
row stays unclassified, at-rest encryption (`RB-SEC-004`/`RB-SEC-009`) carries
into GA as a disclosed ADR-0001 limitation.

### POST-GA

| Area | Rows |
| --- | --- |
| Desktop/Tauri (PRD Phase 4), its OS matrix, and browser pipe UX | RB-PRD4-001..008, RB-COND-002, RB-PIPE-004 |
| Calls and SFU (PRD Phase 6) | RB-PRD6-001..007 |
| v2 replica protocol | RB-ENTRY-001..007, RB-V2C-001..014, RB-V2D-001..009 |
| Federation and v2 Phase E | RB-V2E-001..004 |
| Availability/always-on nodes (PRD Phase 5) | RB-PRD5-001..007 |
| 10,000-member bootstrap | RB-COND-001 |
| Large-room gossip overlay and N=40/v2 scale gate | RB-NET-001..006, RB-FC-007 |
| PRD Phase 3 task/agent events and agent-workspace surfaces | RB-PRD3-001..009 |
| Live-pipe extensions | RB-PIPE-001..003 |
| End-to-end content encryption and removal-triggered key rotation (OD-1, RB-1.2; v0.2 headline) | RB-SEC-002, RB-SEC-011..014, RB-ADM-005 |
| Independent human/firm security review and final candidate audit (OD-2, RB-1.3) | RB-SEC-010, RB-FC-009 |

None of these rows is part of the published SDK's GA surface: the overlay is
behind the non-default `gossip_overlay` feature of `iroh-rooms-net` (not enabled
by the SDK or CLI), and `iroh-rooms-v2-core` is outside the SDK's dependency
closure. The v0.1 GA CLI platform matrix is the four shipped triples, so
`RB-COND-002` (desktop matrix, including the Windows decision) moves with
Phase 4.

`RB-COND-001`'s cohort-blocking column is left unedited in this amendment so
the change does not touch the line adjacent to the pending `RB-DEC-001` edit
(#360); this section controls its classification.

### Unclassified pending maintainer decision

- New trust features not yet in the shipped SDK/CLI: `RB-SEC-003` device
  verification, `RB-SEC-005` recovery phrase, `RB-SEC-007` agent trust levels,
  `RB-SEC-008` room-level pipe policies, `RB-SEC-015` blocklist/moderation.
- `RB-PRD3-010` user-facing room export.
- Every `RB-CPT-*` row (separate `pi-rooms-cockpit` repository).
- `RB-COH-001..008`: open decision OD-3, whether the external cohort runs
  against the v0.1 GA candidate before the GA decision.
- #162 remains open as the v2 trigger tracker.

## RB-1.2 — OD-1 content encryption decision

On 2026-11-15 the maintainer decided open decision OD-1
([`ADR-0014`](docs/decisions/ADR-0014-v0.1-ga-sdk-cli-small-rooms-scope.md#open-decisions-for-the-maintainer))
as option (b): **v0.1 GA ships room content as plaintext, encrypted in transit
only** (iroh QUIC/TLS between admitted peers), with the gap disclosed in the
README, `SECURITY.md`, and threat model T27. End-to-end content encryption and
removal-triggered key rotation are the v0.2 headline.

- `RB-SEC-002` (#250), `RB-SEC-011` (#268), `RB-SEC-012..014` (#280, #286,
  #295), and `RB-ADM-005` (#297) become `POST-GA`; their rows, dependencies,
  and evidence are unchanged.
- `RB-REL-007` (#343) stays `GA-P0` and must include an encryption-claims
  entry: no CLI, SDK, or doc surface may imply content confidentiality beyond
  transport encryption to admitted peers.
- `build_member_removed_with_rotation` moves from the stable
  `iroh_rooms::room` re-exports to `iroh_rooms::experimental::rotation`, so the
  published stable tier does not promise rotation.
- OD-2 (`RB-SEC-010` #344, `RB-FC-009` #353) narrows to confirming the
  encryption path is unreachable in the shipped configuration, not a review of
  the cryptography.

## RB-1.3 — OD-2 security review decision

On 2026-11-15 the maintainer decided open decision OD-2
([`ADR-0014`](docs/decisions/ADR-0014-v0.1-ga-sdk-cli-small-rooms-scope.md#open-decisions-for-the-maintainer)):
**v0.1 GA ships with WAIVER-PBETA-0003 ("no independent security review
completed") explicitly re-accepted and disclosed**, backed by an internal
structured self-review instead of an independent reviewer or firm.

- `RB-SEC-010` (#344) and `RB-FC-009` (#353) become `POST-GA`; their rows,
  dependencies, and evidence are unchanged, and they remain mandatory for the
  full roadmap.
- For the v0.1 GA candidate only, gate item "Independent security review" is
  satisfied by the dated acceptance of WAIVER-PBETA-0003 together with the
  completed self-review record
  [`docs/security/v0.1-self-review.md`](docs/security/v0.1-self-review.md):
  threat-model rows T01–T30 with status, evidence, and owner; the
  reachability inventory tied to `RB-REL-007` (#343), including the OD-1
  unreachable-encryption check; a required `cargo-deny` check; and a fuzz
  target for the CBOR and frame parsers. This is the same narrow exception
  RB-1.1 made for WAIVER-PBETA-0001/0002; it does not complete any RB-1 row.
- WAIVER-PBETA-0003's expiry moves from "before public beta expansion or GA"
  to **before v0.2 or any public beta expansion, whichever comes first**. The
  approved rc.5 sign-off record is not rewritten (preservation invariants);
  this amendment supersedes its expiry additively.
- `RB-FC-003` (#352) reads accordingly for the v0.1 GA candidate.
- `SECURITY.md` and the v0.1 GA release notes state plainly that v0.1 GA had
  no independent security review, only the internal structured self-review.

## RB-1.4 — CLI member removal

On 2026-11-15 the maintainer made CLI member removal a v0.1 GA P0 item (option
B of the member-removal review). The SDK already exposes `build_member_removed`
and `build_member_left` and the core and net layers already enforce removal,
but the CLI had no command to author either event, so a CLI operator could not
remove a member or leave a room, and the T07/T09 mitigations were unreachable
from the shipped binary.

- New row `RB-GA-002` (`GA-P0`, #379): `room remove <ROOM_ID> <MEMBER_ID>` (admin
  only; `insufficient_role` otherwise) and `room leave <ROOM_ID>`, following
  the `room send` pattern (SDK builder, validate, fold, push, store).
- Docs must use the honest wording: removed members lose access to new
  messages, files and pipes once peers see the removal; they keep what they
  already had. There is no key rotation in v0.1 GA (RB-1.2).
- Threat-model rows T07, T09, and T18 are reworded accordingly; their status
  does not change. T09's disposition for v0.1 GA is admin removal plus sticky
  departure, with no blocklist.
- `RB-SEC-002` (#250) and `RB-ADM-005` (#297) stay `POST-GA` (RB-1.2); this
  amendment does not bring rotation or admin transfer into GA.

## RB-1.5 — release signing role

On 2026-11-15 the maintainer approved Sigstore keyless signing for release
artifacts (design for #237 / #248,
<https://github.com/kortiene/iroh-room/issues/237#issuecomment-6896502278>).
There is no long-lived signing key to
custody. `RB-REL-001`'s hardware / external role is therefore
**release-workflow owner + release-environment reviewer** (the `release`
environment's required reviewer). The row's deliverable, dependencies,
completion evidence, status, and cohort-blocking flag are unchanged. Status
stays `not-started`: the signing workflow is on `main`, and no signed release
has been published, so the signature-asset evidence does not exist yet.

## Governance and entry decisions

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-GOV-001 | Activate RB-1 and the roadmap-before-cohort decision. | PRE | ADR-0012 | `iroh-room` / #228 | — | Merged ROADMAP + accepted ADR | Maintainer approval | complete | Yes |
| RB-GOV-002 | Make README, CONTRIBUTING, production readiness, and cohort guidance point to RB-1 and its ordering. | PRE | ADR-0012 | `iroh-room` / #228 | RB-GOV-001 | Live-pointer conformance tests and merged docs | Maintainer review | complete | Yes |
| RB-GOV-003 | Post the explicit Trigger 3 `FIRED` record on #162 without closing it. | PRE | #162; ADR-0004 | `iroh-room` / #162 | RB-GOV-001 | Dated comment with qualified audit scope and current test evidence | Maintainer | complete | Yes |
| RB-GOV-004 | Create dependency-ordered GitHub issues and milestones for every required RB-1 row, including the Cockpit repository boundary. | PRE | ADR-0012 | `iroh-room` / #229 | RB-GOV-003 | Every required row links an owning issue; dependency audit complete | Maintainer/project operator | complete | Yes |
| RB-DEC-001 | Decide D-9 schema evolution: lock-step rejection or forward-compatible handling, including registry and fixture rules. | PRE | `specs/content-and-moderation-event-schemas.md`; PR #186 | `iroh-room` / #232 | RB-GOV-004 | Accepted [`ADR-0013`](docs/decisions/ADR-0013-schema-evolution-lock-step.md) (lock-step; terminal v1 registry; additive advertised-floor v2 revisions); conformance matrix and product-event v2 placement recorded in ADR-0013; follow-up placement comments linked from #232 | Protocol reviewer | complete | Yes |
| RB-COND-001 | Measure the complete 10,000-member bootstrap and re-evaluate proof-carrying light-client mode if it exceeds 5 MiB. | COND | #160; ADR-0004; ADR-0008 | `iroh-room` / #302 | RB-V2C-011..012 | Full-state fixture includes governance, proof, checkpoint, and member data; measured encoded/decoded sizes are recorded; an over-target result has an accepted implementation disposition | Performance operator | partial | Yes |
| RB-COND-002 | Define the supported desktop OS/architecture matrix before Phase 4; explicitly decide Windows rather than inferring support from Tauri. | COND | PRD v0.3 Phase 4; release matrix | `iroh-room` / #231 | RB-GOV-004 | Accepted platform decision and mapped qualification rows | Release/platform owners | not-started | No (POST-GA, RB-1.1) |

## Phase C entry corrections and admin-fork hardening

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-ENTRY-001 | Make moderation envelope and body stream identifiers consistent and tested. | PRE | PR #184 review | `iroh-room` / #242 | RB-DEC-001 | Positive/negative body and envelope tests | Protocol reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-ENTRY-002 | Classify canonical top-level non-map content as `InvalidContent` under the approved D-9 taxonomy. | PRE | PR #184 review | `iroh-room` / #243 | RB-DEC-001 | Exact rejection-vector and taxonomy tests | Protocol reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-ENTRY-003 | Replace taxonomy source-substring scanning with syntax-aware or explicit reachability coverage that excludes comments/tests. | PRE | PR #184 review | `iroh-room` / #244 | RB-DEC-001 | Mutation/regression test proves false textual references do not satisfy coverage | Independent reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-ENTRY-004 | Structurally parse and validate the frozen member-Merkle JSON fixture and its proof objects. | PRE | PR #184 review | `iroh-room` / #233 | RB-GOV-004 | Schema parse, canonical decode, root/proof verification tests | Independent vector reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-ENTRY-005 | Add an immutable successor/erratum for counter 3's `activi` proof copy without editing the frozen v1 artifact. | PRE | `v2-member-merkle.json`; frozen-vector policy | `iroh-room` / #245 | RB-ENTRY-004 | Bumped fixture/schema, protocol note, independently recomputed proof | Independent vector operator | not-started | No (POST-GA, RB-1.1) |
| RB-ENTRY-006 | Correct the recorded audit claim so it names the actual governance/fork-only, non-author-agent scope. | PRE | PR #186 review; v2 audit | `iroh-room` / #234 | RB-GOV-004 | Additive erratum and corrected live pointers; audit record itself not rewritten | Maintainer + reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-ENTRY-007 | Reconcile legacy candidate domains, records, checkpoints, and fork envelopes through additive successor domains/schemas. | PRE | PR #186 review; #156/#157/#159/#161 | `iroh-room` / #246 | RB-DEC-001 | Domain registry, successor codecs/vectors, frozen legacy compatibility tests | Protocol + independent vector reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-ADM-001 | Cache divergence results only by `membership_projection_generation`. | PRE | ADR-0006 follow-up | `iroh-room` / #230 | RB-GOV-004 | Cache invalidation tests across chat and membership mutations; T18 unchanged | Performance reviewer | not-started | Yes |
| RB-ADM-002 | Implement the divergence severity predicate instead of unconditional CRITICAL severity. | PRE | ADR-0005/0006 | `iroh-room` / #240 | RB-ADM-001 | Severity conformance tests | Security reviewer | not-started | Yes |
| RB-ADM-003 | Surface durable divergence/trust decisions to operators through CLI diagnostics and audit. | PRE | ADR-0006 | `iroh-room` / #260 | RB-ADM-002; RB-UP-105 | CLI/audit integration tests and operator runbook | Operations reviewer | not-started | Yes |
| RB-ADM-004 | Detect cross-partition divergence without accepting forged advertised state. | PRE | ADR-0005/0006 | `iroh-room` / #241 | RB-ADM-001 | Partition/rejoin adversarial conformance suite | Independent security reviewer | not-started | Yes |
| RB-ADM-005 | Resolve cross-epoch rotation conflicts on concurrent removals while preserving full carrier-chain `admin_seq` and T18. | PRE | ADR-0006; content-key rotation spec | `iroh-room` / #297 | RB-SEC-002; RB-SEC-014 | Conflict/restart/arrival-order tests plus Vector 18 compatibility | Protocol/security reviewer | not-started | No (POST-GA, RB-1.2) |

## Security, storage, rotation, and audit

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-SEC-001 | Implement native invite revocation and product-visible revocation status. | PRE | PRD v0.3 §13.5; ADR-0002 | `iroh-room` / #249 | RB-DEC-001 | Wire/store/CLI/SDK tests including offline and replay cases | Independent security reviewer | not-started | Yes |
| RB-SEC-002 | Make member removal trigger end-to-end room-key rotation through shipped CLI and SDK surfaces. | PRE | PRD v0.3 §13.5; content-key rotation spec | `iroh-room` / #250 | RB-DEC-001 | Production caller, multi-peer removal, restart, and removed-member denial tests | Security reviewer | partial | No (POST-GA, RB-1.2) |
| RB-SEC-003 | Implement device verification with user-visible state and recovery behavior. | PRE | PRD v0.3 §13.5 | `iroh-room` / #251 | RB-DEC-001 | Protocol/store/CLI/SDK verification tests | Independent security reviewer | not-started | Yes |
| RB-SEC-004 | Encrypt the local database, including persisted room keys, with migration and key-management procedures. | PRE | PRD v0.3 §13.5; ADR-0001 | `iroh-room` / #266 | RB-SEC-003 | At-rest inspection, migration, wrong-key, backup, and recovery tests | Security + data-recovery operators | not-started | Yes |
| RB-SEC-005 | Implement and document the recovery phrase lifecycle. | PRE | PRD v0.3 §13.5 | `iroh-room` / #279 | RB-SEC-003; RB-SEC-004 | Generate/restore/rotate/loss tests and user runbook | Independent security reviewer | not-started | Yes |
| RB-SEC-006 | Implement secure backup and restore, including encrypted-secret handling and recovery drills. | PRE | PRD v0.3 §13.5; production readiness | `iroh-room` / #285 | RB-SEC-004; RB-SEC-005 | Fresh-host restore, rollback, corruption, and secret-leak tests | Independent recovery operator | partial | Yes |
| RB-SEC-007 | Implement explicit trust levels for agents. | PRE | PRD v0.3 §13.5 | `iroh-room` / #252 | RB-DEC-001 | Authorization matrix, CLI/SDK, and migration tests | Agent-security reviewer | not-started | Yes |
| RB-SEC-008 | Implement room-level live-pipe policies beyond per-command allowlists. | PRE | PRD v0.3 §13.5 | `iroh-room` / #267 | RB-DEC-001; RB-SEC-007 | Policy lifecycle and fail-closed forwarding tests | Pipe-security reviewer | not-started | Yes |
| RB-SEC-009 | Encrypt remaining local storage, including blobs, audit data, and recovery metadata. | PRE | PRD v0.3 §13.5 | `iroh-room` / #294 | RB-SEC-004; RB-SEC-006 | At-rest inspection, streaming, restart, and recovery tests | Security/data operator | not-started | Yes |
| RB-SEC-010 | Complete an independent human/firm pre-candidate review of all shipped trust surfaces. | PRE | PRD v0.3 §13.5; production readiness | `iroh-room` / #344 | Every cohort-blocking PRE/COND row whose deliverable changes a shipped trust surface, excluding RB-SEC-010 | Published scope, findings, dispositions, and remediation verification | Independent human reviewer/firm | not-started | No (POST-GA, RB-1.3) |
| RB-SEC-011 | Make encrypted content writes reachable in shipping configuration and enforce the room encryption floor. | PRE | content-key rotation spec; threat model | `iroh-room` / #268 | RB-SEC-002 | CLI/SDK creation, mixed-floor rejection, upgrade, and restart tests | Security reviewer | partial | No (POST-GA, RB-1.2) |
| RB-SEC-012 | Emit and page `WantKeyHistory` requests in production and complete newcomer/history recovery. | PRE | content-key rotation spec | `iroh-room` / #280 | RB-SEC-011 | Real session history recovery, pagination, authorization, and restart tests | Network/security reviewer | partial | No (POST-GA, RB-1.2) |
| RB-SEC-013 | Replace zero key-distribution attribution IDs with durable, deterministic source provenance. | PRE | content-key rotation spec | `iroh-room` / #286 | RB-SEC-012 | Conflict ordering and history provenance tests | Protocol reviewer | not-started | No (POST-GA, RB-1.2) |
| RB-SEC-014 | Persist unresolved poison state, epoch commitments, and conflict candidates across crashes. | PRE | content-key rotation spec | `iroh-room` / #295 | RB-SEC-013 | Unresolvable-conflict crash/reopen tests; no silent unpoisoning | Independent security reviewer | not-started | No (POST-GA, RB-1.2) |
| RB-SEC-015 | Implement the v1/v2 blocklist and moderation product surface selected by D-9. | PRE | PRD v0.3 §13.1; threat model T09; #158 | `iroh-room` / #253 | RB-DEC-001 | Protocol, CLI/SDK, abuse, and migration tests | Abuse/security reviewer | not-started | Yes |
| RB-SEC-016 | Implement audit retention, rotation, and privacy controls. | PRE | ADR-0003; production readiness | `iroh-room` / #239 | RB-GOV-004 | Bounded retention, rotation, permissions, and redaction tests | Operations/privacy reviewer | not-started | Yes |
| RB-SEC-017 | Add tamper-evident audit sequencing and verification. | PRE | ADR-0003; production readiness | `iroh-room` / #254 | RB-SEC-016 | Mutation/truncation/reordering detection tests | Independent security reviewer | not-started | Yes |
| RB-SEC-018 | Add privacy-preserving audit export and incident-reconstruction workflow. | PRE | production readiness; release operations | `iroh-room` / #269 | RB-SEC-016; RB-SEC-017 | Redacted export fixture and incident drill | Operations/privacy reviewer | not-started | Yes |

## Network, overlay, CLI, SDK, and release surfaces

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-NET-001 | Bound `actor.subscribe` so one gossip mesh attempt cannot wedge indefinitely. | PRE | #192 | `iroh-room` / #192 | RB-GOV-004 | Timeout/cancellation/retry tests | Network reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-NET-002 | Emit durable mesh spawn, success, failure, and retry audit events. | PRE | #192 | `iroh-room` / #192 | RB-NET-001; RB-SEC-016 | Audit vocabulary and integration tests | Operations reviewer | partial | No (POST-GA, RB-1.1) |
| RB-NET-003 | Detect an installed zero-neighbor mesh and resubscribe without churn. | PRE | #192 | `iroh-room` / #192 | RB-NET-001 | Neighbor-loss/recovery and rate-bound tests | Network reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-NET-004 | Build the structured real-network overlay measurement harness. | PRE | #214; overlay measurement spec | `iroh-room` / #214 | RB-NET-001..003 | Harness tests and schema-validated JSON output | Network test operator | not-started | No (POST-GA, RB-1.1) |
| RB-NET-005 | Run O1 home↔CGNAT and O2 home↔VPS in both directions with `K=5`, path observation, IPv6/VPN trap controls, and no cascade. | PRE | #214; overlay measurement spec | `iroh-room` / #214 | RB-NET-004 | Per-run JSON + rolled-up findings on the candidate behavior | Home, CGNAT, VPS operators | not-started | No (POST-GA, RB-1.1) |
| RB-NET-006 | Enable `gossip_overlay` and the paired larger-room cap in shipping binaries only after #192/#214 pass. | PRE | gossip overlay spec; #192/#214 | `iroh-room` / #283 | RB-NET-005 | Shipping-feature tests, docs, and clean N=5/10/20/40 matrix | Release/network reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-NET-007 | Run the missing home-NAT→CGNAT reverse Gate A leg. | PRE | Gate A results; production readiness | `iroh-room` / #235 | RB-GOV-004 | Dated natural + relay-only structured results | Home + CGNAT operators | not-started | Yes |
| RB-NET-008 | Acquire cellular access and rerun forced-relay throughput with a representative transfer. | PRE | Gate A results; WAIVER-PBETA-0001 | `iroh-room` / #236 | RB-GOV-004 | Dated cellular natural/relay results meeting or dispositioning the measured threshold through implementation | Cellular hardware/operator | blocked | Yes |
| RB-UP-101 | Implement deterministic `iroh-rooms room list --json`. | PRE | room-list spec; Cockpit UP-101 | `iroh-room` / #255 | RB-DEC-001 | CLI JSON schema and multi-room tests | — | not-started | Yes |
| RB-UP-102 | Implement live `room tail --json` as all-event NDJSON. | PRE | live-tail spec; Cockpit UP-102 | `iroh-room` / #256 | RB-DEC-001 | Session/event NDJSON schema, flushing, attribution, and live tests | — | not-started | Yes |
| RB-UP-103 | Render every supported live event type in text mode. | PRE | Cockpit UP-103; release readiness | `iroh-room` / #257 | RB-DEC-001 | Golden text output and live-tail tests | — | not-started | Yes |
| RB-UP-104 | Implement `pipe list --json`. | PRE | Cockpit UP-104 | `iroh-room` / #258 | RB-DEC-001 | Stable schema and CLI tests | — | not-started | Yes |
| RB-UP-105 | Emit structured diagnostics through a stable JSON block. | PRE | Cockpit UP-105 | `iroh-room` / #259 | RB-DEC-001 | Direct/relay/offline/auth/config schemas and tests | Network reviewer | not-started | Yes |
| RB-UP-106 | Add runtime counters for pipe bytes, fetch progress, and sync/backfill state. | PRE | Cockpit UP-106 | `iroh-room` / #270 | RB-UP-105 | Monotonicity/reset/privacy and integration tests | Performance reviewer | not-started | Yes |
| RB-CLI-001 | Ship a guided local smoke-check command for new users that validates identity, local data access, diagnostics, and a minimal room workflow without exposing secrets. | PRE | production readiness P2 | `iroh-room` / #261 | RB-UP-105 | CLI unit/integration tests plus a fresh-host documented success, failure, and redaction rehearsal | Independent fresh-host documentation operator | not-started | Yes |
| RB-SDK-001 | Reconcile SDK façade coverage for the complete pre-desktop v1/core/network/CLI capability set named by this row. | PRE | `docs/sdk-coverage.md`; production readiness | `iroh-room` / #306 | RB-ENTRY-001..007; RB-ADM-001..005; RB-SEC-001..009; RB-SEC-011..018; RB-NET-001..008; RB-UP-101..106; RB-CLI-001; RB-PIPE-001..003; RB-PRD3-001..007; RB-PRD3-010 | Updated coverage table maps every dependency's SDK-visible surface, stable-surface tests pass, and no accidental direct imports remain; later rows own their explicitly named SDK evidence | SDK reviewer | partial | Yes |
| RB-SDK-002 | Publish the supported Rust SDK crates with versioning, ownership, and release procedure. | PRE | PRD v0.3 Phase 2; production readiness | `iroh-room` / #313 | RB-SDK-001; RB-REL-001..003 | Registry publication dry run + published candidate packages | Crate/release owners | not-started | Yes |
| RB-REL-001 | Sign every supported release artifact and publish verification instructions. | PRE | production readiness; threat model | `iroh-room` / #237 | RB-GOV-004 | Signature assets and independent verification on each artifact | release-workflow owner + release-environment reviewer | partial; item (9) of https://github.com/kortiene/iroh-room/issues/352#issuecomment-7012603384: "Verification on GitHub-hosted runners in a separate, read-only workflow counts as independent verification." | Yes |
| RB-REL-002 | Publish verifiable build provenance for every supported artifact. | PRE | production readiness; release operations | `iroh-room` / #248 | RB-REL-001 | Provenance attestations bind source commit, workflow, target, and digest | Provenance reviewer | partial | Yes |
| RB-REL-003 | Enforce dependency vulnerability, license, and release-supply-chain gates. | PRE | production readiness; threat model | `iroh-room` / #238 | RB-GOV-004 | CI policy and clean final reports with dispositions | Security/release reviewer | partial | Yes |
| RB-REL-004 | Qualify every supported OS/architecture natively, including downloaded artifact execution. | PRE | release matrix; platform audits | `iroh-room` / #265 | RB-COND-002; RB-REL-001..002 | Native verify plus checksum/signature/install smoke per target | Linux, Intel Mac, Apple Silicon operators | partial | Yes |
| RB-REL-005 | Verify install, upgrade, rollback, uninstall, and prior-candidate data continuity. | PRE | install/uninstall; compatibility; production readiness | `iroh-room` / #293 | RB-REL-004; RB-SEC-006 | Published-artifact lifecycle matrix and data fixtures | Platform/data operators | partial | Yes |
| RB-REL-006 | Complete privacy-preserving support, bug-report, incident, and rollback procedures. | PRE | production readiness; release operations | `iroh-room` / #299 | RB-SEC-018; RB-REL-005 | Dry-run support and incident exercises | Support/operations owners | partial | Yes |
| RB-REL-007 | Audit and remove every advertised-but-unreachable capability. | PRE | README limitations; production readiness | `iroh-room` / #343 | All implementation rows | CLI/SDK/docs reachability inventory with tests | Independent release reviewer | not-started | Yes |

## Current-product future surfaces

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-PIPE-001 | Add explicitly authorized terminal-sharing sessions. | PRE | PRD v0.3 §9.3 | `iroh-room` / #273 | RB-SEC-008 | Protocol/CLI/SDK security tests and operator warnings | Independent pipe-security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PIPE-002 | Add explicitly authorized Unix-socket forwarding. | PRE | PRD v0.3 §9.3 | `iroh-room` / #274 | RB-SEC-008 | Path/permission/authz and cross-platform tests | Linux/macOS operators | not-started | No (POST-GA, RB-1.1) |
| RB-PIPE-003 | Add bounded multiplexed live-pipe services. | PRE | PRD v0.3 §9.3 | `iroh-room` / #275 | RB-SEC-008; RB-UP-106 | Stream-budget, fairness, teardown, and abuse tests | Performance/security reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-PIPE-004 | Add browser-native live-pipe UX without weakening loopback/authz defaults. | PRE | PRD v0.3 §9.3 | `iroh-room` / #315 | RB-PIPE-003; RB-PRD4-001 | Browser/desktop integration and security tests | Desktop/browser operators | not-started | No (POST-GA, RB-1.1) |

## PRD Phase 3 — Agent Workspace Alpha

`PRD3` is distinct from v2 Phase C and every Cockpit milestone.

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-PRD3-001 | Implement `task.created`. | PRE | PRD v0.3 §9.1/Phase 3 | `iroh-room` / #247 | RB-DEC-001 | Schema, authorization, CLI/SDK, vectors, migration tests | Protocol reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-002 | Implement `task.updated`. | PRE | PRD v0.3 §9.1/Phase 3 | `iroh-room` / #262 | RB-PRD3-001 | State/concurrency, CLI/SDK, vectors, migration tests | Protocol reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-003 | Implement `agent.output`. | PRE | PRD v0.3 §9.1/Phase 3 | `iroh-room` / #263 | RB-DEC-001; RB-SEC-007 | Schema, authorization, display, vectors, migration tests | Agent-security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-004 | Implement `agent.error`. | PRE | PRD v0.3 §9.1/Phase 3 | `iroh-room` / #276 | RB-PRD3-003 | Schema, display, redaction, vectors, migration tests | Agent-security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-005 | Implement `agent.artifact.shared`. | PRE | PRD v0.3 §9.1/Phase 3 | `iroh-room` / #264 | RB-DEC-001; RB-SEC-007 | Blob authorization, schema, CLI/SDK, vectors, migration tests | Agent-security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-006 | Implement `agent.review.requested`. | PRE | PRD v0.3 §9.1/Phase 3 | `iroh-room` / #277 | RB-PRD3-005 | Review lifecycle, authorization, CLI/SDK, vectors, migration tests | Agent-product reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-007 | Stream live agent logs through authorized pipes. | PRE | PRD v0.3 Phase 3 | `iroh-room` / #278 | RB-SEC-008; RB-PRD3-003 | Backpressure, redaction, authz, reconnect, and UX tests | Agent/pipe reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-008 | Serve agent-generated web previews through authorized pipes. | PRE | PRD v0.3 Phase 3 | `iroh-room` / #325 | RB-SEC-008; RB-PIPE-004 | End-to-end preview, teardown, warning, and authz tests | Desktop/browser operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-009 | Integrate MX-Agent and MX-Loom through documented stable SDK surfaces. | PRE | PRD v0.3 Phase 3 | `iroh-room` / #336 | RB-SDK-001; RB-PRD3-001..008 | Two integration fixtures/demos and compatibility policy | External integration operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD3-010 | Implement a user-facing room export with integrity and secret-exclusion guarantees. | PRE | PRD v0.3 Phase 3/§14 | `iroh-room` / #291 | RB-SEC-004; RB-SEC-006 | Export/import, integrity, redaction, and versioning tests | Data-recovery operator | not-started | Yes |

## PRD Phase 4 — Desktop Prototype

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-PRD4-001 | Build the Tauri desktop application shell and distribution pipeline. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #308 | RB-COND-002; RB-SDK-001 | Native builds, packaging, startup, update, and security tests | Supported desktop operators | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-002 | Implement the desktop room list. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #316 | RB-PRD4-001; RB-UP-101 | Multi-room state and UX tests | Desktop operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-003 | Implement the chat timeline. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #317 | RB-PRD4-001; RB-UP-102..103 | All-event, pagination, restart, and accessibility tests | Desktop/product reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-004 | Implement the file panel. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #318 | RB-PRD4-001 | Share/fetch/availability/integrity UX tests | Desktop operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-005 | Implement the pipe panel. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #319 | RB-PRD4-001; RB-SEC-008; RB-UP-104 | Open/connect/close/policy/audit UX tests | Pipe-security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-006 | Implement agent cards and trust/status controls. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #320 | RB-PRD4-001; RB-SEC-007; RB-PRD3-003..006 | Agent lifecycle and permission UX tests | Agent-product reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-007 | Implement QR invite creation and scanning with secret-safe handling. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #321 | RB-PRD4-001; RB-SEC-001 | Expiry/revocation/wrong-identity/camera-permission tests | Camera-equipped desktop operators | not-started | No (POST-GA, RB-1.1) |
| RB-PRD4-008 | Implement local database management, migration, backup, restore, and recovery UI. | PRE | PRD v0.3 Phase 4 | `iroh-room` / #322 | RB-SEC-004..006; RB-PRD4-001 | Destructive-action safeguards and recovery drills | Data-recovery operator | not-started | No (POST-GA, RB-1.1) |

## PRD Phase 5 — Availability Layer

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-PRD5-001 | Ship a user-owned always-on node. | PRE | PRD v0.3 Phase 5/§14 | `iroh-room` / #303 | RB-V2C-009; RB-REL-005 | Deployment, restart, upgrade, monitoring, and failure tests | Always-on host operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD5-002 | Ship a room archive peer. | PRE | PRD v0.3 Phase 5/§14 | `iroh-room` / #309 | RB-PRD5-001 | Authorization, retention, recovery, and availability tests | Archive operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD5-003 | Define and implement room blob-pinning policy. | PRE | PRD v0.3 Phase 5 | `iroh-room` / #310 | RB-PRD5-001 | Policy lifecycle, quota, eviction, and integrity tests | Storage operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD5-004 | Implement improved offline catch-up with bounded resources and clear progress. | PRE | PRD v0.3 Phase 5 | `iroh-room` / #311 | RB-V2C-005..012; RB-UP-106 | Long-offline, partial provider, restart, and progress tests | Slow/lossy network operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD5-005 | Ship optional self-hosted relay configuration and operations guidance. | PRE | PRD v0.3 Phase 5/§14 | `iroh-room` / #304 | RB-NET-005; RB-REL-006 | Deployment, failover, upgrade, abuse, and real-network tests | Relay host/operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD5-006 | Ship an organization-owned availability node profile. | PRE | PRD v0.3 §14 | `iroh-room` / #323 | RB-PRD5-001..003 | Multi-room policy, isolation, operations, and recovery tests | Organization-node operator | not-started | No (POST-GA, RB-1.1) |
| RB-PRD5-007 | Ship the optional storage-pinning service. | PRE | PRD v0.3 §14 | `iroh-room` / #326 | RB-PRD5-003; RB-PRD5-006 | Authz, quota, billing-free operation, integrity, and recovery tests | Storage-service operator | not-started | No (POST-GA, RB-1.1) |

## PRD Phase 6 — Calls Prototype and future call stages

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-PRD6-001 | Implement call signaling with `call.started` and `call.ended` room events. | PRE | PRD v0.3 §9.4/Phase 6 | `iroh-room` / #284 | RB-DEC-001; RB-PRD3-001..006 | Schemas, authorization, vectors, migration, and reconnect tests | Protocol reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD6-002 | Implement authorized 1:1 WebRTC calls. | PRE | PRD v0.3 §9.4/Phase 6 | `iroh-room` / #292 | RB-PRD6-001 | Audio/video, NAT/relay, teardown, and permission tests | Two media-device operators | not-started | No (POST-GA, RB-1.1) |
| RB-PRD6-003 | Render call state in the room timeline. | PRE | PRD v0.3 Phase 6 | `iroh-room` / #327 | RB-PRD4-003; RB-PRD6-001 | Ordering, missed-event, restart, and accessibility tests | Desktop/product reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-PRD6-004 | Implement the basic desktop call UI. | PRE | PRD v0.3 Phase 6 | `iroh-room` / #337 | RB-PRD4-001; RB-PRD6-002..003 | Permission, device selection, failure, and teardown tests | Supported desktop/media operators | not-started | No (POST-GA, RB-1.1) |
| RB-PRD6-005 | Complete the small-group WebRTC mesh experiment and implement the accepted result. | PRE | PRD v0.3 §9.4/Phase 6 | `iroh-room` / #298 | RB-PRD6-002 | Three-plus endpoint performance/network results and product decision | Three-plus media operators | not-started | No (POST-GA, RB-1.1) |
| RB-PRD6-006 | Implement the peer-selected SFU stage after direct/small-group validation. | PRE | PRD v0.3 §9.4 | `iroh-room` / #305 | RB-PRD6-005 | Selection/failover/authz/privacy/performance evidence | SFU host + media operators | not-started | No (POST-GA, RB-1.1) |
| RB-PRD6-007 | Implement the optional Jitsi bridge without making Jitsi authoritative for room membership. | PRE | PRD v0.3 §9.4 | `iroh-room` / #312 | RB-PRD6-006 | Bridge authz, lifecycle, metadata, failure, and operations tests | Jitsi host/operator | not-started | No (POST-GA, RB-1.1) |

## V2 Phase C — network and storage

`V2C` means ADR-0004/#134 Phase C. It is unrelated to the production-readiness
"Milestone C" and Cockpit phases.

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-V2C-001 | Register `/iroh-rooms/event/2` on the router without changing v1 ALPN behavior. | PRE | #134; #162 | `iroh-room` / #271 | RB-GOV-003; RB-ENTRY-001..007 | Dual-ALPN routing and downgrade/replay tests | Protocol/network reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-002 | Implement the authenticated v2 replica/client handshake. | PRE | #134 Phase C | `iroh-room` / #281 | RB-V2C-001; RB-ENTRY-007 | Golden handshake vectors and adversarial tests | Independent protocol reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-003 | Implement v2 publish and subscription flows with bounded fanout/backpressure. | PRE | #134 Phase C | `iroh-room` / #287 | RB-V2C-002 | Conformance, backpressure, reconnect, and authorization tests | Network/performance reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-004 | Implement bounded batch fetch for content and governance dependencies. | PRE | #134 Phase C | `iroh-room` / #288 | RB-V2C-002 | Missing/duplicate/malicious batch tests | Network reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-005 | Implement #155 range reconciliation and the spec-owned envelope. | PRE | #155; ADR-0007 | `iroh-room` / #289 | RB-ENTRY-007; RB-V2C-002 | Additive codecs/vectors, convergence, partition, and bound tests | Independent vector/network reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-006 | Implement stream checkpoints and range-resume behavior. | PRE | #134 Phase C; #155 | `iroh-room` / #296 | RB-V2C-005 | Checkpoint/resume/crash/compaction conformance tests | Storage/network reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-007 | Implement #157's stable replica identity separately from rotatable Iroh Endpoint IDs. | PRE | #157; ADR-0009 | `iroh-room` / #272 | RB-ENTRY-007 | Successor descriptor codecs/vectors and endpoint-rotation tests | Independent vector reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-008 | Implement #156 durability descriptors, receipt classes, and full receipt/descriptor vectors. | PRE | #156; ADR-0010 | `iroh-room` / #282 | RB-ENTRY-007; RB-V2C-007 | Additive codecs/vectors and fsync/group-commit/replicated-WAL tests | Storage + independent vector reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-009 | Implement and operate 3–7 durable community replicas. | PRE | #134 §5/Phase C | `iroh-room` / #300 | RB-V2C-003..008 | Multi-node deployment, restart, durability, monitoring, and upgrade evidence | 3–7 hosts and test operators | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-010 | Implement replica persistence receipts and W-quorum assembly. | PRE | #134 Phase C; #156 | `iroh-room` / #307 | RB-V2C-008..009 | Quorum, timeout, conflicting receipt, and durability tests | Multi-replica operator | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-011 | Implement #161 successor governance snapshots and admin-transition proofs. | PRE | #161; ADR-0008 | `iroh-room` / #290 | RB-ENTRY-007; RB-V2C-008 | Additive snapshot/proof codecs/vectors and transition tests | Independent vector reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-012 | Implement snapshot startup, segmented storage, and compaction without losing verification history. | PRE | #134 Phase C; #161 | `iroh-room` / #301 | RB-V2C-006; RB-V2C-011 | Crash, corruption, compaction, bootstrap, and retained-proof tests | Storage/recovery operator | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-013 | Implement #159 replica genesis, policy, readiness, handoff, quarantine, incident, recovery, and cancellation records. | PRE | #159; ADR-0011 | `iroh-room` / #314 | RB-ENTRY-007; RB-V2C-007..012 | Additive public vectors, store/crash conformance, and lifecycle tests | Independent vector + operations reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-V2C-014 | Cut and document an internal v2 protocol beta after Phase C, without starting the external cohort. | PRE | #162 migration trigger | `iroh-room` / #324 | RB-V2C-001..013 | Immutable tag/artifacts, compatibility statement, and beta scope | Release/protocol owners | not-started | No (POST-GA, RB-1.1) |

## V2 Phase D — production hardening

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-V2D-001 | Test and harden multi-replica failure, partition, recovery, and equivocation. | PRE | #134 Phase D | `iroh-room` / #328 | RB-V2C-014 | Deterministic fault suite plus real multi-host results | 3–7 replica operators | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-002 | Ship admin-threshold configuration and transition UX. | PRE | #134 Phase D | `iroh-room` / #329 | RB-V2C-014 | CLI/SDK/desktop, quorum-loss, and transition tests | Governance/security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-003 | Ship replica/community recovery UX. | PRE | #134 Phase D; #159 | `iroh-room` / #330 | RB-V2C-013..014 | Incident, handoff, quarantine, and recovery drills | Operations/recovery operators | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-004 | Integrate native invite revocation into v2 UX and replica enforcement. | PRE | #134 Phase D | `iroh-room` / #331 | RB-SEC-001; RB-V2C-014 | Cross-version revocation and offline-replica tests | Security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-005 | Integrate key rotation into v2 UX and replica lifecycle. | PRE | #134 Phase D | `iroh-room` / #332 | RB-SEC-002; RB-SEC-011..014; RB-V2C-014 | Rotation, late replica, removal, recovery, and migration tests | Independent security reviewer | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-006 | Implement abuse controls, moderation, quotas, alerts, and operator dashboards. | PRE | #134 Phase D | `iroh-room` / #333 | RB-SEC-015; RB-UP-105..106; RB-V2C-014 | Adversarial load, quota, privacy, and operator-response tests | Abuse/performance/operations reviewers | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-007 | Pass direct, relay, lossy, mobile, slow-link, and constrained-device phase gates. | PRE | #134 Phase D | `iroh-room` / #334 | RB-V2C-014; RB-NET-007..008 | Structured multi-environment result corpus | Cellular, home, CGNAT, VPS, constrained-device operators | blocked | No (POST-GA, RB-1.1) |
| RB-V2D-008 | Implement and dry-run v1→v2 migration with rollback and immutable v1 verification. | PRE | #134 §20/Phase D; #162 | `iroh-room` / #335 | RB-V2C-014; RB-REL-005 | Published prior-candidate fixtures, dry runs, rollback, and byte-preservation tests | Migration/data operator | not-started | No (POST-GA, RB-1.1) |
| RB-V2D-009 | Complete an independent pre-Phase-E review of v2 core, runtime, store, network, migration, and operator trust surfaces. | PRE | #134 hardening; production readiness | `iroh-room` / #338 | RB-V2D-001..008 | Human/firm audit, finding dispositions, and remediation verification; RB-FC-009 owns the final-candidate delta | Independent human reviewer/firm | not-started | No (POST-GA, RB-1.1) |

## V2 Phase E — future architecture included before cohort

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-V2E-001 | Specify and implement cross-community federation. | PRE | #134 Phase E; #162 | `iroh-room` / #339 | RB-V2D-009 | Accepted spec, separate wire version where required, multi-community conformance | Independent protocol reviewer + community-node operators | not-started | No (POST-GA, RB-1.1) |
| RB-V2E-002 | Specify and implement per-stream replica groups. | PRE | #134 Phase E | `iroh-room` / #340 | RB-V2D-009 | Placement, failover, authorization, and migration tests | Multi-region replica operators | not-started | No (POST-GA, RB-1.1) |
| RB-V2E-003 | Specify and implement regional placement policy. | PRE | #134 Phase E | `iroh-room` / #341 | RB-V2E-002 | Region-loss, policy, privacy, and operations evidence | Multi-region infrastructure/operator | not-started | No (POST-GA, RB-1.1) |
| RB-V2E-004 | Specify and implement mandatory group encryption for the Phase E profile. | PRE | #134 Phase E; #162 | `iroh-room` / #342 | RB-V2D-005; RB-V2E-001..003 | Independent vectors, membership/rotation/recovery tests, security audit | Independent cryptography reviewer | not-started | No (POST-GA, RB-1.1) |

## Cockpit and upstream ergonomics — enumerated backlog and explicit futures

The Cockpit backlog's optional labels do not remove items from RB-1. CPT work
requires creation of `kortiene/pi-rooms-cockpit`; its six UP items are the
`RB-UP-101` through `RB-UP-106` rows above and remain in this repository. Those
six rows plus the 30 CPT rows below are the complete enumerated table. The
source's "34 issues" footer is arithmetically stale; no listed item is dropped
to make the count match.

The source separately names four optional future deliverables. They remain
included through stable rows: invite-revocation surfacing is `RB-SEC-001`,
multi-room simultaneous sessions are `RB-CPT-013`, pipe traffic sparklines are
`RB-CPT-018` with `RB-UP-106`, and live Pi-theme matching is `RB-CPT-012`.

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-CPT-001 | Pi package scaffolding and placement. | PRE | CPT-001 | `pi-rooms-cockpit` / [#1](https://github.com/kortiene/pi-rooms-cockpit/issues/1) | RB-GOV-004 | CPT-001 AC and load-smoke | Pi package reviewer | blocked | Yes |
| RB-CPT-002 | CLI adapter for binary/home resolution and coded-error decoding. | PRE | CPT-002 | `pi-rooms-cockpit` / [#4](https://github.com/kortiene/pi-rooms-cockpit/issues/4) | RB-CPT-001 | CPT-002 unit/redaction tests | — | blocked | Yes |
| RB-CPT-003 | Managed child-process supervisor. | PRE | CPT-003 | `pi-rooms-cockpit` / [#8](https://github.com/kortiene/pi-rooms-cockpit/issues/8) | RB-CPT-002 | Lifecycle, backoff, cleanup, and no-orphan tests | — | blocked | Yes |
| RB-CPT-004 | Pinned-format parser library and fixture corpus. | PRE | CPT-004 | `pi-rooms-cockpit` / [#5](https://github.com/kortiene/pi-rooms-cockpit/issues/5) | RB-CPT-001 | Parser fixtures and drift tests | — | blocked | Yes |
| RB-CPT-005 | Cockpit CI and test harness. | PRE | CPT-005 | `pi-rooms-cockpit` / [#6](https://github.com/kortiene/pi-rooms-cockpit/issues/6) | RB-CPT-001 | CI/load-smoke green | — | blocked | Yes |
| RB-CPT-006 | Cockpit secret-redaction and trust posture. | PRE | CPT-006 | `pi-rooms-cockpit` / [#2](https://github.com/kortiene/pi-rooms-cockpit/issues/2) | RB-GOV-004 | Redaction/threat tests and security guidance | Security reviewer | blocked | Yes |
| RB-CPT-010 | UX specification and ASCII mockups for all panels. | PRE | CPT-010 | `pi-rooms-cockpit` / [#3](https://github.com/kortiene/pi-rooms-cockpit/issues/3) | RB-GOV-004 | Approved panel specification | Product/design reviewer | blocked | Yes |
| RB-CPT-011 | Dashboard shell, layout engine, and keyboard navigation. | PRE | CPT-011 | `pi-rooms-cockpit` / [#7](https://github.com/kortiene/pi-rooms-cockpit/issues/7) | RB-CPT-010 | Interaction and accessibility tests | Terminal UX reviewer | blocked | Yes |
| RB-CPT-012 | Cockpit visual language, theme mapping, and live Pi-theme matching. | PRE | CPT-012; Cockpit optional-future list | `pi-rooms-cockpit` / [#9](https://github.com/kortiene/pi-rooms-cockpit/issues/9) | RB-CPT-010..011 | Theme switching, role mapping, contrast, and golden evidence | Design reviewer | blocked | Yes |
| RB-CPT-013 | State model, room registry, and simultaneous sessions across multiple rooms. | PRE | CPT-013; Cockpit optional-future list | `pi-rooms-cockpit` / [#10](https://github.com/kortiene/pi-rooms-cockpit/issues/10) | RB-CPT-002 | Concurrent multi-room session, isolation, resource-bound, and restart tests | — | blocked | Yes |
| RB-CPT-014 | Members and Agents panel. | PRE | CPT-014 | `pi-rooms-cockpit` / [#14](https://github.com/kortiene/pi-rooms-cockpit/issues/14) | RB-CPT-004; RB-CPT-011; RB-CPT-013 | Panel/golden/live-room tests | — | blocked | Yes |
| RB-CPT-015 | Live Activity offline-JSON diff feed. | PRE | CPT-015 | `pi-rooms-cockpit` / [#15](https://github.com/kortiene/pi-rooms-cockpit/issues/15) | RB-CPT-004; RB-CPT-011; RB-CPT-013 | Diff/order/dedup/golden tests | — | blocked | Yes |
| RB-CPT-016 | Managed live-tail session presence. | PRE | CPT-016 | `pi-rooms-cockpit` / [#16](https://github.com/kortiene/pi-rooms-cockpit/issues/16) | RB-CPT-003..004 | Reconnect/cleanup/presence tests | — | blocked | Yes |
| RB-CPT-017 | Files panel for list, share, and fetch. | PRE | CPT-017 | `pi-rooms-cockpit` / [#11](https://github.com/kortiene/pi-rooms-cockpit/issues/11) | RB-CPT-004; RB-CPT-011 | Integrity/availability/golden tests | — | blocked | Yes |
| RB-CPT-018 | Pipes panel, audit feed, and pipe traffic sparklines. | PRE | CPT-018; Cockpit optional-future list | `pi-rooms-cockpit` / [#17](https://github.com/kortiene/pi-rooms-cockpit/issues/17) | RB-CPT-003; RB-CPT-011; RB-UP-106 | Pipe/audit/security/golden tests plus counter-derived sparkline accuracy and reset handling | Security/performance reviewers | blocked | Yes |
| RB-CPT-019 | Network and Availability panel. | PRE | CPT-019 | `pi-rooms-cockpit` / [#20](https://github.com/kortiene/pi-rooms-cockpit/issues/20) | RB-CPT-004; RB-CPT-016 | Path/availability/limitation tests | Network reviewer | blocked | Yes |
| RB-CPT-020 | Alerts panel for coded errors, warnings, and next actions. | PRE | CPT-020 | `pi-rooms-cockpit` / [#12](https://github.com/kortiene/pi-rooms-cockpit/issues/12) | RB-CPT-004; RB-CPT-011 | Taxonomy/golden/action tests | — | blocked | Yes |
| RB-CPT-021 | Command bar and invite-ticket dialog. | PRE | CPT-021 | `pi-rooms-cockpit` / [#18](https://github.com/kortiene/pi-rooms-cockpit/issues/18) | RB-CPT-011; RB-CPT-013 | Command/authz/secret-handling tests | Security reviewer | blocked | Yes |
| RB-CPT-022 | LLM tools suite. | PRE | CPT-022 | `pi-rooms-cockpit` / [#19](https://github.com/kortiene/pi-rooms-cockpit/issues/19) | RB-CPT-002; RB-CPT-013 | Tool schemas, permission, redaction, and e2e tests | Agent-security reviewer | blocked | Yes |
| RB-CPT-023 | Ambient footer pill and mini-feed mode. | PRE | CPT-023 | `pi-rooms-cockpit` / [#21](https://github.com/kortiene/pi-rooms-cockpit/issues/21) | RB-CPT-015..016 | Responsive/golden/activity tests | — | blocked | Yes |
| RB-CPT-024 | Responsive narrow-terminal behavior. | PRE | CPT-024 | `pi-rooms-cockpit` / [#13](https://github.com/kortiene/pi-rooms-cockpit/issues/13) | RB-CPT-011 | Width/focus/accessibility golden tests | Terminal UX reviewer | blocked | Yes |
| RB-CPT-025 | Golden render tests for all panels. | PRE | CPT-025 | `pi-rooms-cockpit` / [#22](https://github.com/kortiene/pi-rooms-cockpit/issues/22) | RB-CPT-011..020 | Stable cross-terminal golden suite | — | blocked | Yes |
| RB-CPT-030 | Sidecar protocol and lifecycle ADR. | PRE | CPT-030 | `pi-rooms-cockpit` / [#23](https://github.com/kortiene/pi-rooms-cockpit/issues/23) | RB-CPT-010..025 | Accepted ADR and threat review | Cross-repo/SDK reviewers | blocked | Yes |
| RB-CPT-031 | `cockpit-sidecar` Rust crate skeleton. | PRE | CPT-031 | `pi-rooms-cockpit` / [#26](https://github.com/kortiene/pi-rooms-cockpit/issues/26) | RB-CPT-030 | Build/test/package skeleton | Rust/SDK reviewer | blocked | Yes |
| RB-CPT-032 | Sidecar live streams for events, peers, and agents. | PRE | CPT-032 | `pi-rooms-cockpit` / [#28](https://github.com/kortiene/pi-rooms-cockpit/issues/28) | RB-CPT-031 | NDJSON/backpressure/reconnect/e2e tests | Network/agent reviewers | blocked | Yes |
| RB-CPT-033 | Backend abstraction for CLI and sidecar modes. | PRE | CPT-033 | `pi-rooms-cockpit` / [#27](https://github.com/kortiene/pi-rooms-cockpit/issues/27) | RB-CPT-030 | Shared conformance suite across both backends | — | blocked | Yes |
| RB-CPT-034 | Sidecar build and distribution story. | PRE | CPT-034 | `pi-rooms-cockpit` / [#29](https://github.com/kortiene/pi-rooms-cockpit/issues/29) | RB-CPT-031 | Signed/provenanced multi-platform artifacts and install tests | Release/platform owners | blocked | Yes |
| RB-CPT-040 | Visual polish and performance pass. | PRE | CPT-040 | `pi-rooms-cockpit` / [#24](https://github.com/kortiene/pi-rooms-cockpit/issues/24) | RB-CPT-010..025 | Render/latency/resource budgets and UX review | Performance/design reviewers | blocked | Yes |
| RB-CPT-041 | README, setup guide, demo script, and recordings. | PRE | CPT-041 | `pi-rooms-cockpit` / [#25](https://github.com/kortiene/pi-rooms-cockpit/issues/25) | RB-CPT-010..025 | Fresh-host documentation rehearsal | Independent documentation operator | blocked | Yes |
| RB-CPT-043 | Cockpit release-readiness checklist and honest limitations. | PRE | CPT-043 | `pi-rooms-cockpit` / [#30](https://github.com/kortiene/pi-rooms-cockpit/issues/30) | RB-UP-101..106; RB-CPT-001..006; RB-CPT-010..025; RB-CPT-030..034; RB-CPT-040..041 | Candidate checklist passes with no invented metrics | Release reviewer | blocked | Yes |

## Recurring roadmap-complete candidate gates

These gates apply to the exact tagged cohort candidate. A material security,
protocol, migration, platform, networking, or product fix creates a new
candidate and reruns every affected gate.

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-FC-001 | Tag one roadmap-complete cohort candidate and bind every evidence record to its exact commit and artifacts. | FC | ADR-0012; milestone #8 | `iroh-room` / #345 | All PRE/COND rows | Immutable tag, manifest, source commit, artifact digests | Release owner | not-started | Yes |
| RB-FC-002 | Run `scripts/verify.sh` and every release-readiness online tier with no skips. | FC | release readiness | `iroh-room` / #346 | RB-FC-001 | Green logs on supported CI and named operator hosts | Linux/Intel Mac/Apple Silicon operators | not-started | Yes |
| RB-FC-003 | Pass production preflight and all manual P0 sign-offs without using a waiver as row completion. For the v0.1 GA candidate only, the P0.3 independent-review sign-off is met by the completed internal self-review record plus the accepted WAIVER-PBETA-0003 (RB-1.3). | FC | production readiness | `iroh-room` / #352 | RB-FC-002 | Candidate production sign-off | Security/network/data/release owners | not-started | Yes |
| RB-FC-004 | Download, verify signature/provenance, install, execute, upgrade, rollback, and uninstall every supported artifact natively. | FC | release operations | `iroh-room` / #347 | RB-REL-001..005; RB-FC-001 | Per-platform artifact records | Native platform operators | not-started | Yes |
| RB-FC-005 | Refresh Gate A and overlay real-network evidence against candidate-equivalent transport code. | FC | Gate A; #214 | `iroh-room` / #348 | RB-NET-005..008; RB-FC-001 | Structured home/CGNAT/VPS/cellular corpus | Network/hardware operators | not-started | Yes |
| RB-FC-006 | Pass v1/v2 compatibility, immutable-vector, prior-candidate, migration, and rollback gates. | FC | compatibility; ADR-0004 | `iroh-room` / #349 | RB-V2D-008; RB-FC-001 | Fixture and published-binary migration evidence | Migration operator | not-started | Yes |
| RB-FC-007 | Pass N=40 and v2 scale/performance/fault budgets on declared reference environments. | FC | #134; N=40 specs | `iroh-room` / #350 | RB-V2D-001; RB-FC-001 | Structured performance/fault result corpus | Cloud/reference-hardware operators | not-started | No (POST-GA, RB-1.1) |
| RB-FC-008 | Complete backup, restore, recovery-phrase, encrypted-storage, and disaster-recovery drills. | FC | production readiness; security roadmap | `iroh-room` / #351 | RB-SEC-004..006; RB-SEC-009; RB-FC-001 | Fresh-host and corruption recovery records | Independent recovery operator | not-started | Yes |
| RB-FC-009 | Complete final independent security audits and close every cohort-blocking finding. | FC | production readiness; RB-SEC-010; RB-V2D-009 | `iroh-room` / #353 | RB-FC-001..008 | Signed scope, reports, dispositions, and final delta | Independent human reviewers/firm | not-started | No (POST-GA, RB-1.3) |
| RB-FC-010 | Reconcile README, guides, limitations, release notes, support, and protocol claims to the candidate. | FC | release/production readiness | `iroh-room` / #354 | RB-FC-001..009 | Documentation conformance and fresh-user rehearsal | Independent documentation operator | not-started | Yes |
| RB-FC-011 | Confirm no open cohort-blocking issue and no advertised-but-unreachable functionality. | FC | ADR-0012 | `iroh-room` / #355 | RB-FC-010; RB-REL-007 | Issue/reachability audit signed by release owner | Independent release reviewer | not-started | Yes |
| RB-FC-012 | Record final protocol, security, networking, platform, data, operations, and release-owner sign-offs. | FC | milestone #8 | `iroh-room` / #356 | RB-FC-001..011 | Named sign-off table linked from candidate | Named reviewers/owners | not-started | Yes |

## Cohort-dependent and post-cohort work

These rows do not count as missing pre-cohort roadmap implementation. They
remain mandatory before GA.

| ID | Exact deliverable | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-COH-001 | Confirm three named external participants, bind milestone #8 to the exact candidate, and assign its 30-day due date. | COHORT | milestone #8; #100 | `iroh-room` / #100 | RB-FC-012 | Three confirmations, tag, activation date, due date | Three external participants | deferred | Yes |
| RB-COH-002 | Run three external builder attempts during the 30-day final cohort. | COHORT | #100 | `iroh-room` / #100 | RB-COH-001 | Attempt records with consent and redaction | External participants | deferred | No |
| RB-COH-003 | Collect human DX timings, comprehension, workflow, and product-use evidence. | COHORT | PRD §17.2; production readiness | `iroh-room` / #100 | RB-COH-002 | Dated cohort evidence and analysis | External participants/product observer | deferred | No |
| RB-COH-004 | Triage cohort feedback and complete every GA-blocking discovered fix. | COHORT | #101 | `iroh-room` / #101 | RB-COH-002 | Linked issues/PRs, dispositions, and residual-risk record | Maintainer + reviewers | deferred | No |
| RB-COH-005 | Record accepted limitations against the actual cohort candidate rather than rc.1. | COHORT | #102 | `iroh-room` / #102 | RB-COH-002..004 | Candidate-specific limitations record | Maintainer/product reviewer | deferred | No |
| RB-COH-006 | Decide whether and when to enable GitHub Discussions. | COHORT | #103 | `iroh-room` / #103 | RB-COH-002 | Dated decision and operating boundary | Maintainer/community owner | deferred | No |
| RB-COH-007 | If cohort findings cause material fixes, cut a replacement candidate, rerun affected FC gates, and restart or extend the cohort window when required; otherwise record that no replacement is required. | COHORT | ADR-0012 | `iroh-room` / #357 | RB-COH-004 | Replacement tag, gate mapping, and dated window decision, or a dated no-replacement-required disposition linked to triage evidence | Release owner + affected reviewers | deferred | No |
| RB-COH-008 | Make and publish the dated GA decision after cohort evidence and final candidate gates. | COHORT | production readiness; milestone #8 | `iroh-room` / #358 | RB-COH-003..007 | GA decision memo or explicit no-GA disposition | Maintainer + named sign-off owners | deferred | No |

## Explicit RB-1 non-goals

| ID | Exact boundary | Class | Source | Owner / issue | Dependencies | Completion evidence | Hardware / external role | Status | Cohort-blocking |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| RB-NG-001 | Do not adopt `iroh-docs` as the direct event-plane surface. | NON-GOAL | #162; ADR-0007 | `iroh-room` / #162 | RB-GOV-001 | Active baseline records the boundary | — | proposed | No |
| RB-NG-002 | Do not reinterpret or rewrite v1 history, frozen vectors, legacy signed artifacts, or approved release records. | NON-GOAL | ADR-0004; frozen-vector policy | `iroh-room` / baseline | RB-GOV-001 | Preservation invariants and compatibility gates | — | proposed | No |
| RB-NG-003 | Do not implement full Matrix client-server API compatibility. | NON-GOAL | PRD v0.3 §7.3 | `iroh-room` / baseline | RB-GOV-001 | Active baseline records the boundary | — | proposed | No |
| RB-NG-004 | Do not build a full Jitsi replacement or general-purpose custom SFU; the peer-selected SFU and Jitsi bridge remain included. | NON-GOAL | PRD v0.3 §9.4 | `iroh-room` / baseline | RB-GOV-001 | Active baseline records the distinction | — | proposed | No |
| RB-NG-005 | Do not add public rooms, public discovery, or global usernames. | NON-GOAL | PRD v0.3 §7.3 | `iroh-room` / baseline | RB-GOV-001 | Active baseline records the boundary | — | proposed | No |
| RB-NG-006 | Do not add native mobile applications or push notifications; mobile hardware remains required for network verification. | NON-GOAL | PRD v0.3 §7.3; #134 Phase D | `iroh-room` / baseline | RB-GOV-001 | Active baseline records product vs verification distinction | — | proposed | No |
| RB-NG-007 | Do not add billing, an enterprise admin console, or public app-store-ready UX. | NON-GOAL | PRD v0.3 §7.3 | `iroh-room` / baseline | RB-GOV-001 | Active baseline records the boundary | — | proposed | No |
| RB-NG-008 | Do not claim guaranteed offline delivery; implement and measure the included availability improvements honestly. | NON-GOAL | PRD v0.3 §14 | `iroh-room` / baseline | RB-GOV-001 | Product language and final docs preserve the boundary | — | proposed | No |
| RB-NG-009 | Do not claim anonymous credentials or full metadata privacy. | NON-GOAL | PRD v0.3 §13.4 | `iroh-room` / baseline | RB-GOV-001 | Threat model and final docs preserve the boundary | — | proposed | No |

## Dependency-ordered execution

The milestone numbers below belong to RB-1 and do not reuse the repository's
historical `Phase C` or Cockpit milestone names.

1. **RB0 — Baseline:** merge RB-GOV-001/002.
2. **RB1 — Governance and acquisition:** post RB-GOV-003, materialize
   RB-GOV-004, start cellular/Apple-Silicon/replica/call-hardware acquisition,
   and engage independent security reviewers.
3. **RB2 — Decisions and correctness:** land D-9, PR #184/#186 corrections,
   additive vectors/domains, admin-divergence hardening, rotation hardening, and
   #192 lifecycle residuals.
4. **RB3 — Parallel implementation rails:**
   - #214 harness/evidence, then shipping overlay;
   - v2 Phase C, internal v2 beta, Phase D, then Phase E;
   - PRD Phase 3, then Phase 4/Cockpit, Phase 5, and Phase 6;
   - v1 security/storage/audit, CLI, SDK, platform, and release work.
5. **RB4 — Convergence:** all cohort-blocking PRE/COND rows complete.
6. **RB5 — Candidate:** cut the roadmap-complete cohort candidate and pass all
   FC gates.
7. **RB6 — External validation:** confirm three participants and run milestone
   #8 for 30 days.
8. **RB7 — Post-cohort:** fix findings, re-cut/re-gate as required, and make the
   GA decision.

Under RB-1.1 the v0.1 GA track (SDK + CLI) runs ahead of this order: complete the `GA-P0`
gate and as many `GA-P1` targets as practical, tag the v0.1 GA candidate, then
pass its scoped `FC` gates. `POST-GA` rows continue on the RB3 rails above
afterwards.

## Amendment log

| Version | Date | Change | Approval |
| --- | --- | --- | --- |
| RB-1.0 | 2026-08-06 | Initial finite baseline compiled from current repository and GitHub sources. | Proposed by ADR-0012; active on merge |
| RB-1.1 | 2026-11-14 | Define the v0.1 GA candidate as the SDK (`iroh-rooms`) and the CLI built on it, for small private rooms; add `GA-P0`/`GA-P1`/`POST-GA` classes and row `RB-GA-001`; make SDK publication (RB-SDK-001/002) and RB-DEC-001 `GA-P0`; reclassify desktop (with its OS matrix), calls/SFU, v2, federation, availability, the 10k bootstrap, the large-room overlay, PRD3 task/agent work, and pipe extensions `POST-GA`; record open decisions OD-1..OD-3. | Proposed by ADR-0014; maintainer decisions 2026-11-14 (scope; SDK + CLI); active on merge |
| RB-1.2 | 2026-11-15 | Decide OD-1 as option (b): v0.1 GA ships plaintext room content, encrypted in transit only, disclosed; reclassify `RB-SEC-002`, `RB-SEC-011..014`, and `RB-ADM-005` `POST-GA`; keep `RB-REL-007` `GA-P0` with an encryption-claims entry; move the rotation builder to the SDK `experimental` tier; narrow OD-2 to unreachability of the encryption path. | Maintainer decision 2026-11-15 (OD-1, option b); recorded in ADR-0014; active on merge |
| RB-1.3 | 2026-11-15 | Decide OD-2: v0.1 GA ships with WAIVER-PBETA-0003 re-accepted and disclosed, backed by an internal structured self-review (`docs/security/v0.1-self-review.md`); reclassify `RB-SEC-010` and `RB-FC-009` `POST-GA`; let the accepted waiver plus the self-review record satisfy the security-review gate item for the v0.1 GA candidate only; align `RB-FC-003`; move the waiver's expiry to before v0.2 or public beta expansion. | Maintainer decision 2026-11-15 (OD-2, waiver option); recorded in ADR-0014; active on merge |
| RB-1.4 | 2026-11-15 | Add `GA-P0` row `RB-GA-002` (#379): CLI `room remove` (admin only) and `room leave`, with honest removal wording (peers enforce removal once they see it; removed members keep what they already had); reword threat-model T07/T09/T18; record T09's v0.1 GA disposition as admin removal plus sticky departure. | Maintainer decision 2026-11-15 (member removal, option B, GA-P0); active on merge |
| RB-1.5 | 2026-11-15 | `RB-REL-001`: hardware / external role becomes "release-workflow owner + release-environment reviewer". Keyless Sigstore signing has no signing key to custody. Deliverable, dependencies, evidence, status (`not-started`), and cohort-blocking flag unchanged. | Maintainer decision 2026-11-15 (signed-release design D8), <https://github.com/kortiene/iroh-room/issues/237#issuecomment-6896502278>; active on merge |
