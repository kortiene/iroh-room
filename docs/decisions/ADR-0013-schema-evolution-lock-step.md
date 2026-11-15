# ADR-0013: Schema Evolution Is Lock-Step Within a Profile

- **Date:** 2026-11-14
- **Status:** Proposed until the merge that introduces this ADR; accepted by
  that merge
- **Decision owner:** Maintainer, with protocol review (RB-1 `RB-DEC-001`)
- **Related:** RB-1 [`RB-DEC-001`](https://github.com/kortiene/iroh-room/issues/232)
  and [`RB-SEC-015`](https://github.com/kortiene/iroh-room/issues/253);
  [`ADR-0004`](../post-ga/decisions/ADR-0004-accept-v2-large-community-architecture.md);
  [`ADR-0012`](ADR-0012-roadmap-completion-before-external-cohort.md);
  `docs/audits/feature-complete-audit-2026-07-02.md` P-26 / decision D-9;
  `docs/protocol.md` §6/§9/§11;
  `specs/content-and-moderation-event-schemas.md` (§4 D9, §5, §10 R1, §11 OQ-6);
  issues #134 (§6.4, §7.3, §20), #153, #158, #162, #184, #186

## Context

The repository has an enforced hard gate — audit **P-26 / decision D-9** —
stating that *no `schema_version: 2` and no new event-type registry work may
land until a schema-evolution ADR is written*. The gate exists because of the
**schema-evolution trap** (`docs/protocol.md` §11, audit risk #4): the protocol
derives all room state from a locally validated append-only event set, and
every member must reach the *same* set. If one binary accepts an event that
another binary rejects, the room silently diverges — the same class of failure
as the admin same-seq collision in #191/#211, but caused by software skew
instead of malicious forks.

Current state at this decision:

1. **v1 is shipped and strict.** The transport envelope rejects any
   `v != 1` as `non_canonical_encoding` (`event/wire.rs`); the signed event
   rejects any `schema_version != 1` as `unknown_schema_version`; unknown
   event types are rejected `unknown_event_type`; strict per-type content
   validation rejects unknown/missing/wrong-typed/out-of-bound fields as
   `invalid_content`. The v1 registry is the 12 shipped types
   (`room.created`, `member.invited`, `member.joined`, `member.left`,
   `member.removed`, `message.text`, `file.shared`, `pipe.opened`,
   `pipe.closed`, `agent.status`, `content.encrypted`,
   `member.key_distribution`).
2. **A real v1 fleet exists.** `v0.1.0-rc.1` through `rc.5` binaries are
   published and in narrow supervised use. These binaries cannot be upgraded
   atomically and have no registry-negotiation surface.
3. **v1 bytes are frozen.** The 20 Tier-1 conformance vectors, SQLite
   migration fixtures, and all shipped signed history are immutable by policy
   (ADR-0012 preservation requirements; RB-NG-002).
4. **v2 is accepted architecture, not shipped code.** ADR-0004 ratifies #134:
   a new profile with a new ALPN (`/iroh-rooms/event/2`), a new
   `ContentEventBody` envelope and content-kind registry, governance records,
   and one-way v1→v2 migration (#134 §20.2). #134 §6.4 already mandates that
   signed records reject "unknown mandatory schema versions", and §7.3
   mandates that unknown governance operations are "rejected, not ignored".
5. **v2 bytes are already frozen as fixtures.** The three golden-vector sets
   under `crates/iroh-rooms-v2-core/tests/golden/` (issue #153) pin wire
   encodings, domain strings, identifiers, and typed rejection codes; any
   change to an existing expectation "requires an explicit schema-version
   bump" per the frozen-vector policy.
6. **Product work is waiting on this decision.** PRD v0.3 §9.1 lists
   post-MVP event types (`task.created`, `task.updated`, `agent.output`,
   `agent.error`, `agent.artifact.shared`, `agent.review.requested`,
   `call.started`, `call.ended`); the v2 content/moderation kinds
   (`moderation.block/report/remove`, `message.reaction`,
   `message.edited`) are fully specified but explicitly implementation-blocked
   on this ADR (`specs/content-and-moderation-event-schemas.md` §10 R1);
   `RB-SEC-015` requires D-9 to *select* the v1/v2 blocklist and moderation
   surface; and ADR-0012 names D-9 "the first technical decision after issue
   materialization" on which "Phase C and new post-MVP event types depend".

The choice in front of the repository is the one the audit posed: **lock-step
rejection** (every schema or registry change ships as an explicit versioned
cutover that the whole room/community performs together, and unknown versions,
types, kinds, and fields are always rejected) versus **forward-compatible
handling** (receivers tolerate and preserve what they do not understand, e.g.
skip-and-relay unknown event types, ignore unknown non-critical fields).

## Decision

Adopt **lock-step rejection** as the permanent schema-evolution policy for
every protocol profile. Specifically:

### R1 — Unknown major versions are always rejected, never preserved

No implementation, in any profile, may accept, store, or re-broadcast an event
whose `schema_version` (or envelope `v`) it does not know. There is no
"parse-opaquely-and-relay" path, no skip-and-persist path, and no
unknown-version tombstone. This is v1's shipped behavior and it becomes
binding policy for v2 and every successor profile. Cross-profile misuse
(v1 bytes on the v2 ALPN or vice versa) is likewise rejected — #134 §20.3's
"negative fixtures for cross-version decoding and mixed-ALPN misuse" are the
pinning evidence, owned by `RB-V2C-001`.

### R2 — Within a version, the registry is closed and strictness never relaxes

The event-type registry (v1) / content-kind registry (v2) of a given
`schema_version` is a closed set. Unknown types are rejected
`unknown_event_type`; unknown kinds are rejected `unknown_content_kind`
(new code, per the content spec §4 D5 / §5 sub-step 5b); unknown fields are
rejected `invalid_content`. "Reject, never ignore" applies at every
discriminant: version, then type, then kind, then kind-minor, then body, in
that order. Adding an ignorable-field mechanism (TLS-extension-style
"non-critical" keys) is prohibited: encode/decode asymmetry breaks
deterministic validation and byte-exact vectors.

### R3 — The v1 registry is terminal

No new event types, content keys, or schema versions are ever added to
`schema_version = 1`. The 12-type registry above is the final v1 registry for
the life of the profile. Consequences:

- **Post-MVP product events ride the v2 schema line only.** The PRD v0.3
  §9.1 post-MVP types (`task.*`, `agent.*`, `call.*`) are authored as v2
  content kinds / v2 registry entries, never as v1 registry additions. Their
  implementation therefore sequences after the v2 schema rail
  (`RB-V2C-001` dual-ALPN routing, `RB-V2C-002` authenticated handshake, and
  the v2 content-kind parser), as recorded in Consequences below.
- **The `RB-SEC-015` surface selection is:** v1 keeps exactly its shipped
  mechanism — `member.removed` plus sticky departure and connect-time
  revocation-on-learn; v1 receives **no** blocklist or moderation event
  types. The stream-scoped moderation kinds (`moderation.block`,
  `moderation.report`, `moderation.remove`) and the blocklist surface are
  v2-only, per `specs/content-and-moderation-event-schemas.md`.
- **Rooms that need new kinds migrate** through the #134 §20.2 one-way
  migration. There is no v1 schema_version 2; the number 2 belongs to the
  v2 profile (#134 §20.1).

Rationale: the deployed rc-fleet binaries hard-reject everything R3 forbids
adding, and they cannot negotiate. Any v1 registry addition would therefore
produce silent same-set divergence between old and new binaries — the exact
trap P-26 was created to prevent — and would require rewriting frozen v1
vectors, which policy forbids.

### R4 — v2 registry growth is additive, versioned, and gated by an advertised floor

Within the v2 profile, the registry may grow, but only under these rules:

1. **Additive only.** A registry revision may add kinds and add kind-minor
   versions. It may never change an existing kind's field set, encoding,
   domain string, or semantics; existing bytes and vectors stay untouched.
   Corrections to a defective frozen kind are made through an additive
   successor kind (the `RB-ENTRY-005`/`RB-ENTRY-007` successor pattern), not
   an in-place edit.
2. **Explicit and versioned.** Every registry revision is a recorded,
   signed governance decision (a new governance operation per #134 §7,
   following the additive-successor discipline for the governance schema
   itself), and it bumps the community's **registry revision** number. The
   content spec's D9 bullet — "additions require a `schema_version` bump +
   registry amendment + taxonomy vector" — is read as: additions require an
   explicit versioned registry amendment plus taxonomy vectors. A *new kind*
   is a registry revision within major version 2; a *change to an existing
   kind's encoding* is the thing that requires a new major `schema_version`.
   This refines the spec's phrasing without relaxing it: nothing is ever
   silent, and every addition is versioned and vectored.
3. **Advertised and enforced as a floor.** Each v2 endpoint advertises its
   supported registry revision in the authenticated v2 handshake
   (`RB-V2C-002`) and in join/membership material; replicas advertise theirs
   the same way. A community's current registry floor is the governance-
   recorded revision, which may rise only after every active member and
   replica advertises support (mirroring the content-key-rotation encryption
   floor pattern, spec `content-key-rotation.md` R2/D8). Writers MUST NOT
   author a kind above the floor — enforced as a local authoring guard in
   CLI/SDK and re-checked by receivers. A receiver that nonetheless sees an
   unknown kind rejects it `unknown_content_kind` and surfaces the drift
   through operator diagnostics (the `RB-ADM-003` surface); it never stores
   or relays it. Defense in depth: even a floor violation degrades to a
   strict rejection, never to divergence.

This keeps the v2 fleet in lock-step through declared, coordinated revisions
while allowing the product to grow — the same discipline as a coordinated
major-version cutover, amortized across additive revisions.

### R5 — Fixture rules

1. **v1 fixtures are immutable forever.** The 20 Tier-1 conformance vectors,
   golden events, and SQLite migration fixtures never change; their tests
   must keep passing unchanged through v1 EOL.
2. **v2 golden vectors are frozen per #153 policy.** A registry revision adds
   *new* fixture entries/files (positive, negative, and round-trip vectors
   for each new kind; a typed negative vector for any new rejection code).
   An edit to an existing entry's bytes, domain strings, identifiers, or
   rejection codes requires an explicit schema-version bump plus a successor
   fixture with a change-log entry — never an in-place edit.
3. **Every registry revision ships with the taxonomy updated.** New rejection
   codes are additive to the `#[non_exhaustive]` `RejectReason` (or the v2
   typed-rejection equivalent), are covered by the taxonomy-completeness
   gate (`tests/conformance/taxonomy.rs` and the v2-core equivalent), and
   update the CLI exit-category mapping in the same change.
4. **Cross-version negatives ship with every profile boundary.** Each
   registry revision and each profile adds cross-version and mixed-ALPN
   negative fixtures per #134 §20.3, so that R1 is continuously enforced.

### R6 — Taxonomy rules (the "approved D-9 taxonomy")

The classification of malformed or unknown input is fixed as follows, and
`RB-ENTRY-001` through `RB-ENTRY-003` implement and test exactly this matrix:

- Envelope-level: a non-map or non-canonical top level, wrong key set, or
  wrong envelope version is `non_canonical_encoding` (envelope `v`) — the
  outer four-key map is the envelope's own schema.
- Event-level: `schema_version != 1` (v1) or an unsupported major (v2) is
  `unknown_schema_version`; an unregistered `event_type` is
  `unknown_event_type`.
- Content-level: **canonical top-level non-map content is `invalid_content`**
  (RB-ENTRY-002); unknown content keys, missing/wrong-typed/out-of-bound
  fields, cross-field violations, and envelope/body scope inconsistencies
  (content spec §5 sub-steps 5e–5g, RB-ENTRY-001) are `invalid_content`.
- Kind-level (v2): an unregistered `kind` is `unknown_content_kind`; an
  unknown kind-minor `version` is `invalid_content`.
- The check order is version → type → kind → kind-minor → body, so exactly
  one code is emitted per event and it is the earliest violated layer.
- The v1 rejection list is otherwise frozen: no v1 rejection code is added,
  renamed, or renumbered. New rejection codes exist only in v2-plus profiles.
  Advisory receive-path *flags* (warning codes that never alter the
  accept/reject set) may still be added to v1 additively — they cannot cause
  divergence because every binary still accepts or rejects the same events.

### Deferred with this decision (explicitly not decided here)

- **Multi-admin signature policy** for moderation actions (content spec §11
  OQ-6): v2 moderation authorization follows the v2 governance state machine
  and its admin-threshold configuration (#134 §7, `RB-V2D-002`); v1 remains
  single-immutable-admin because the v1 registry is terminal. The quorum
  question is owned by #134 §17 fold semantics and `RB-V2D-002`, not by this
  ADR.
- **The exact v2 handshake advertisement encoding and the registry-revision
  governance record shape** are owned by `RB-V2C-002` and the v2 governance
  schema work (`RB-V2C-011` successor snapshots), constrained by this ADR's
  rules.

## Conformance matrix

The matrix below is the `RB-DEC-001` "conformance matrix" deliverable. Each
row is a scenario, the required lock-step behavior, the taxonomy code, the
pinning fixture or policy, and the owning RB-1 row.

| # | Scenario | Required behavior | Code | Pinning fixture / policy | Owning row |
| --- | --- | --- | --- | --- | --- |
| 1 | v1 envelope `v != 1` | reject, never parse | `non_canonical_encoding` | `event/wire.rs` decode; Tier-1 vectors | shipped v1 (P-26 verified) |
| 2 | v1 event `schema_version != 1` | reject | `unknown_schema_version` | `event/constants.rs`/`reject.rs`; Tier-1 vectors | shipped v1 |
| 3 | v1 unknown `event_type` | reject | `unknown_event_type` | §7 registry tests; taxonomy gate | shipped v1 |
| 4 | v1 canonical top-level non-map content | reject | `invalid_content` | new rejection vectors | `RB-ENTRY-002` |
| 5 | v1 unknown/missing/mistyped content field | reject | `invalid_content` | existing strict-content vectors | shipped v1 |
| 6 | v1 event proposed as a v1 registry addition | not permitted at all | — | this ADR R3; review gate | this ADR (discharges P-26) |
| 7 | v2 decoder receives v1 bytes / wrong-ALPN frames | reject | typed v2 rejection | #134 §20.3 mixed-ALPN negatives | `RB-V2C-001` |
| 8 | v2 event, unsupported major `schema_version` | reject | `unknown_schema_version` | #134 §6.4; golden negative vectors | `RB-V2C-002`+ |
| 9 | v2 unregistered content `kind` | reject, never ignore | `unknown_content_kind` | content spec §7 #1 vector | `RB-V2C-003`+ (spec §8 step 6) |
| 10 | v2 known kind, unknown kind-minor `version` | reject | `invalid_content` | content spec §5 5c vectors | `RB-V2C-003`+ |
| 11 | v2 strict body violation (keys/types/caps/enums) | reject | `invalid_content` | content spec §7 #2 vectors | `RB-V2C-003`+ |
| 12 | v2 envelope/body stream-id or scope inconsistency | reject | `invalid_content` | content spec §5 5f–5g; positive/negative tests | `RB-ENTRY-001` |
| 13 | v2 unknown governance operation | reject, not ignore | typed governance rejection | #134 §7.3 | `RB-V2C-011`+ |
| 14 | Writer authors a kind above the community registry floor | authoring guard refuses; receivers reject if attempted | guard + `unknown_content_kind` | floor advertisement in handshake; policy tests | `RB-V2C-002`+, `RB-ADM-003` diagnostics |
| 15 | Registry revision without full vector/taxonomy set | change refused | — | this ADR R5; CI gates | every v2 registry row |
| 16 | Edit to any frozen fixture's expected bytes | forbidden without major schema bump + successor fixture | — | golden README policy; `RB-ENTRY-005` pattern | `RB-ENTRY-005`, `RB-ENTRY-007` |

## Consequences

- **The P-26 / D-9 gate is discharged.** v2 schema work (`schema_version: 2`
  implementations, the content-kind registry, moderation kinds) and new
  post-MVP product event types are no longer blocked by the missing policy;
  they are governed by it. The audit's interim hard rule ("no v2 until the
  ADR") ends with this ADR's acceptance.
- **Sequencing becomes explicit for the product-event rows.** The
  post-MVP event-type work (`RB-PRD3-001`..`RB-PRD3-008` schemas,
  `RB-PRD6-001` call signaling) targets the **v2 registry** and therefore
  follows the v2 schema rail (`RB-V2C-001`/`RB-V2C-002` and the v2
  content-kind parser). The RB-1 dependency expressions for those rows are
  unchanged (their `RB-DEC-001` dependency is what this ADR satisfies); this
  ADR records the placement decision they inherit. Follow-up evidence
  comments linking this ADR are posted on the affected issues.
- **The v1 product surface is now honestly frozen.** `member.removed` +
  sticky departure is the only v1 block mechanism; no v1 blocklist/moderation
  events will ever ship (`RB-SEC-015`'s v1 half is thereby resolved as
  "shipped mechanism, nothing further").
- **Upgrades become first-class operations.** Registry-floor rises, profile
  transitions, and the v1→v2 migration are coordinated, advertised,
  governance-recorded events — the cost of never tolerating the unknown.
  `RB-V2C-014` (internal v2 beta) and `RB-V2D-008` (migration dry-run) are
  the rehearsal surfaces.
- **Cost accepted:** task/call/moderation features reach CLI users only on
  the v2 rail, later than a hypothetical v1 registry addition would have.
  The alternative — silent divergence across a fleet that cannot negotiate —
  is the failure mode this repository exists to prevent.

## Alternatives considered

### Open forward-compatibility (accept, preserve, and relay unknown versions/types)

Rejected. A node that stores or re-broadcasts bytes it cannot validate breaks
the local-validation-before-persistence invariant, exports validation
responsibility to peers, and still diverges semantically (different nodes
fold different accepted sets). It contradicts #134 §6.4/§7.3, the shipped v1
behavior, and the frozen signed-bytes discipline. This is the option the
content spec's §6.4 rule ("rejected, not ignored") already exists to prevent.

### Add post-MVP types to the v1 registry with an "all peers must upgrade" advisory

Rejected. v1 has no negotiation surface: rc.1–rc.5 binaries cannot advertise
or discover registry support, so the advisory is unenforceable and the
divergence is silent. It would also require new v1 content schemas, which the
frozen v1 vectors forbid. This is the precise trap P-26 names.

### Ignorable "non-critical" fields inside a known version

Rejected. Unknown keys must stay fatal: optional-tolerance creates
encode/decode asymmetry (a writer and a strict reader disagree about the same
bytes), breaks byte-exact golden vectors, and reopens the divergence window
inside a single schema version. Field growth happens through new kind-minor
versions or new kinds (R4), which are explicit and vectored.

### Pure major-version lock-step for v2 (every new kind bumps `schema_version`)

Rejected as the default. It would force a full community cutover per product
feature, multiplying migration ceremonies and cross-version negatives for
purely additive changes. R4 keeps the same guarantee (nothing unknown is ever
tolerated; the whole fleet moves in declared lock-step through floor rises)
at additive cost. A major bump remains mandatory for any change to an
existing kind's encoding — the case where silent divergence inside a version
would otherwise return.
