# Protocol Conformance Test Vectors (IR-0003 / #7)

- **Issue:** #7 — [IR-0003] Add protocol conformance test vectors
- **Labels:** `type/test` `area/protocol` `priority/p0` `risk/high`
- **Parent:** #1 (Phase 0 epic)
- **Depends on:** #6 / IR-0002 (canonical signed event model — landed)
- **Traceability:** `PHASE-0-SPIKE.md` → *Protocol Test Vectors* §1–§20 and *Rejection / Flag Taxonomy* (§8); Spike Plan Gate B and Gate D.
- **Kind:** test-only deliverable. **No production code changes.**

---

## 1. Summary

Turn the normative protocol vectors in `PHASE-0-SPIKE.md` into a single, self-contained,
**traceable** conformance suite that another engineer or agent can run with `cargo test`
and that CI runs on every PR. The suite:

1. Reproduces the byte-exact golden values (CSB, `event_id`, signature, `room_id`) exactly.
2. Rejects every non-canonical encoding class with the pinned `§8` reason code.
3. Accepts a clock-skewed-but-valid event **with** an advisory `clock_skew` flag (never a reject).
4. Ignores duplicate events **idempotently** (1× ≡ 1000×).
5. Provides a **machine-checked completeness gate**: every `RejectReason`/`Flag` variant in
   the `§8` taxonomy is either exercised by a named vector or listed on an explicit,
   reviewed deferral list — the build fails if a new reason is added with neither.

The deliverable is a **consolidation + gap-fill + completeness gate**, not a green-field
rewrite. Much of the stateless subset already exists (see §3); this issue makes coverage
*systematic, §-numbered, and provably complete against the taxonomy*, and pins the
stateful fixture-log golden ids the spike deliberately left un-reproduced.

---

## 2. Goal and non-goals

### Goal
A protocol conformance test binary — `crates/iroh-rooms-core/tests/protocol_conformance.rs`
plus a `tests/conformance/` support module — with:

- **One `#[test]` per numbered vector** `§1`–`§20`, named `vector_NN_<slug>`, each asserting
  every `THEN` clause of its vector.
- A **shared fixture module** (`tests/conformance/fixtures.rs`) that deterministically
  builds the Cast, the room, and the full fixture-log DAG (`E_create … E_pipe`, `E_eq_a/b`,
  `E_mal`, plus the `§18`–`§20` hardening events) from one canonical source, so every vector
  derives from identical bytes and the golden ids are pinned in exactly one place.
- A **taxonomy coverage matrix** and a **completeness gate test** (AC5).
- A **traceability table** (vector → test → reason/flag code) embedded as a module doc comment.

### Non-goals
- No changes to production code under `crates/*/src/`. If a vector cannot be expressed
  because a production API is missing or wrong, that is a **finding filed as a separate
  issue**, not a fix in this PR (see §9 Risks).
- No new event types, no schema changes, no new reason codes.
- Not Gate A (real-NAT transport). Transport-level vectors (`§16` blob, `§17` pipe) are
  asserted at the **membership/access-decision** layer (the pure predicates
  `blob_serve_allowed` / `pipe_connect_allowed`), not over live QUIC — the network e2e for
  those lives in `crates/iroh-rooms-net/tests/` and is out of scope here.
- No fuzzing / property testing (may be a follow-up; noted in §9).

---

## 3. Current state (inventory) — what exists, what is missing

Read this before writing code; the point of the issue is *completeness and traceability*,
so start by cataloguing what is already covered and avoid duplicating it.

### Already implemented (dependency #6 and siblings)
- `crates/iroh-rooms-core/src/event/reject.rs` — `RejectReason` (14 variants) and `Flag`
  (3 variants: `ClockSkew`, `Equivocation`, `FromRemovedMember`), each with a `.code()`
  string. `MembershipOracle` trait lives here too.
- `crates/iroh-rooms-core/src/event/validate.rs` — `validate_wire_bytes(...)` (stateless
  `§6` subset → `ValidatedEvent` | `RejectReason`) and `validate_with_membership(...)`
  (`§6` steps 7–8). `ValidationContext`, `ValidatedEvent`. Constants
  `MAX_PREV_EVENTS = 20`, `CLOCK_SKEW_FUTURE_MS = 300_000`.
- `crates/iroh-rooms-core/tests/golden_vectors.rs` — **57 `#[test]` fns** (plus ~11 helper
  fns) covering most of the
  stateless vectors: golden CSB (242 bytes), `event_id` `blake3:c389e2…85a1`, `room_id_A`
  `43c19f2e…16a3`, signature-under-device-not-sender, tampered body, cross-room replay,
  non-canonical cases, `too_many_parents`, unknown schema/type, content-validation
  rejections, clock-skew advisory (threshold tests), and `reject_reason_code_strings_match_spec`
  / `flag_code_strings_match_spec` (**note: the reason-code test currently asserts only 9 of
  the 14 variants** — the five stateful ones are unasserted; the new gate closes this).
- Duplicate idempotency: `tests/store_e2e.rs` (`file_backed_duplicate_across_reopen_is_ignored`)
  and `tests/membership_store_e2e.rs` (dup insert stats) via `InsertOutcome::Duplicate`.
- Stateful behaviour (fold, ordering, equivocation, access gates): scattered across
  `tests/membership_fold.rs`, `tests/sync_convergence.rs`, `tests/e2e_lifecycle.rs`,
  `tests/membership_store_e2e.rs`, `tests/sync_smoke.rs`.

### Gaps this issue closes
1. **No single §-numbered conformance suite.** Stateful vectors `§10`–`§20` are proven
   incidentally by fold/sync tests but are not named or traceable to the spike vectors.
2. **Fixture-log golden ids are un-pinned.** The spike explicitly states the multi-event
   fixture-log ids (`E_create … E_pipe`, `E_eq_a/b`, `E_mal`) were **NOT** independently
   reproduced and "the conformance harness MUST regenerate them from the final content
   schema before they are trusted as golden." No test pins them today.
3. **No completeness gate.** Nothing fails the build when a new `RejectReason`/`Flag` is
   added without a covering vector (AC5).
4. **Advisory-vs-reject discipline not asserted as a set-equality property.** `§20` requires
   that a clock-skewed event yields the **same validated set** as an in-bounds one; this
   should be a first-class assertion, not implied.

---

## 4. Design

### 4.1 Test binary layout (Rust integration-test mechanics)

Rust compiles each top-level file in `tests/` as its own crate; files in **subdirectories**
are not auto-run. Use the standard shared-module pattern:

```
crates/iroh-rooms-core/tests/
  protocol_conformance.rs        # the ONLY test binary; declares the submodules
  conformance/
    mod.rs                       # re-exports; holds the traceability table doc-comment
    fixtures.rs                  # Cast, room, fixture-log DAG, golden-id constants
    serialization.rs             # vectors §1–§7  (stateless, byte-exact)
    idempotency_ordering.rs      # vectors §8–§10 (dedup, buffering, total order)
    membership.rs                # vectors §11–§19 (fold, roles, capability, access)
    advisory.rs                  # vector  §20 + clock-skew + duplicate discipline
    taxonomy.rs                  # the coverage matrix + completeness gate (AC5)
```

`protocol_conformance.rs` is a thin entry:

```rust
//! Executable protocol conformance suite for PHASE-0-SPIKE.md §1–§20 + §8 taxonomy.
//! See `conformance::TRACEABILITY` for the vector → test → reason-code map.
mod conformance {
    pub mod fixtures;
    mod serialization;
    mod idempotency_ordering;
    mod membership;
    mod advisory;
    mod taxonomy;
}
```

**Rationale for one binary:** shared fixtures compile once; the completeness gate can `use`
the same registry the vector tests register into. (An alternative — reusing the existing
`golden_vectors.rs` — is rejected: that file is a flat 57-test grab-bag; a fresh, §-indexed
binary gives clean traceability without a risky refactor of those passing tests. See §8.)

### 4.2 Shared fixtures — the single source of golden truth

`conformance/fixtures.rs` reproduces the spike **Fixtures** block verbatim and exposes it
to every vector:

- **Cast** — five principals (Alice/Bob/Carol/Dave/Mallory), each with an identity key from
  seed `nn` and a device key from seed `nn+1`, all from `seed = byte repeated 32×`. Assert
  the derived public keys equal the spike's hex table (this is `seeds_reproduce_cast_public_keys`,
  already proven for Alice in `golden_vectors.rs`; extend to all five).
- **Room** — `room_nonce = 000102…0e0f`, creator Alice, `created_at = 1_750_000_000_000`;
  derive `room_id_A = 43c19f2e…16a3` and `room_id_B = cad9174a…3494` and assert against the
  spike.
- **Fixture-log builder** — a function per handle that assembles, signs, and validates each
  event, returning `ValidatedEvent` + its `WireEvent` bytes. Reuse the real core builders
  where they exist:
  - `build_room_created` → `E_create`
  - `build_member_invited` → `E_inv_bob`, `E_inv_carol`, `E_inv_dave`
  - `build_member_joined` → `E_join_bob`, `E_join_carol`, `E_join_dave`
  - `build_message_text` → `E_msg_bob`, `E_eq_a`, `E_eq_b`, `E_mal`
  - `build_pipe_opened` → `E_pipe`
  - For types **without** a public builder (`member.removed` → `E_kick_dave`, `member.left`
    → `E_left_bob` for `§19`, `file.shared` → `E_file`, and any `agent.status`), assemble the
    eight-field CBOR map directly and sign via `sign_csb` — mirror the existing
    `signed_bytes_with` / `room_created_wire` helpers in `golden_vectors.rs`. **Do not add
    production builders for these in this PR** (that would be a src change); a local
    `fixtures.rs` assembler is sufficient and correct.
- **Golden-id constants** — `const E_CREATE_ID: &str = "blake3:…";` etc. See §4.3 for how
  these are produced and the mandatory cross-check against the spike.
- **Capability fixtures** — Bob/Dave `invite_id` + `secret` + expected `capability_hash`
  (`7ed58c86…d4a4`, `b944bc4d…447c`), `E_file.content.blob_hash = dd101e8f…f313`,
  `E_pipe.content.allowed_members = [alice_id, bob_id]`.

### 4.3 Golden-id regeneration and the authoritative/derived split

The spike marks two tiers of golden values. The fixtures module **must** treat them differently:

- **Tier 1 — independently reproduced (authoritative, pin as literals, must match spike
  exactly):** golden-event CSB (242 bytes) + prefix `a867636f6e74656e74…`, `event_id`
  `c389e2…85a1`, its signature `98732ece…4f0f`, `device_binding.sig`, `room_id_A`
  `43c19f2e…16a3`, `room_id_B` `cad9174a…3494`, tampered-body id `6267b72c…c75c`, cross-room
  re-signed id `81b6a82b…f057`. A mismatch here is a **hard NO-GO** (the encoder or a
  builder diverged from the trust boundary) — file a blocker, do not "update the golden".
- **Tier 2 — fixture-log ids NOT reproduced by the spike (`E_create … E_pipe`, `E_eq_a/b`,
  `E_mal`, and the `§18`–`§20` events):** the harness **regenerates** these from the real
  content schema/builders, prints them once, and **pins the produced values** as the golden
  constants. Rule: the harness computes the id from the implementation and asserts it equals
  the pinned constant (a regression tripwire), **not** that it equals the spike's placeholder
  hex. Where a produced Tier-2 id differs from the spike's listed hex (expected — the spike's
  content maps were never pinned), record the divergence in a `fixtures.rs` doc comment:
  *"Spike listed `blake3:67bc40…`; regenerated from the landed schema as `blake3:<actual>`;
  the spike value was an un-reproduced placeholder (see PHASE-0-SPIKE.md §Test Vectors
  caveat)."* This is expected and acceptable per the spike's own instruction; the *values
  and derivations* are locked by this PR going forward.

**Procedure to generate Tier-2 ids:** write the vector body to `assert_eq!` against a
`todo`/placeholder, run once, read the actual id from the failure message, paste it into the
`const`, re-run green. Never hand-compute.

### 4.4 The completeness gate (AC5)

`conformance/taxonomy.rs` owns a **coverage registry** and a gate:

- Every vector test registers the `§8` codes it exercises. Simplest robust mechanism: a
  single `const COVERAGE: &[(&str /*reason/flag code*/, &str /*vector id*/)]` table in
  `taxonomy.rs`, hand-maintained next to the vectors (a comment on each vector names its
  codes; the table is the machine-readable mirror).
- `fn every_reason_and_flag_is_covered_or_deferred()`:
  - Build the set of **all** `RejectReason` codes and **all** `Flag` codes. Because Rust has
    no built-in enum reflection, enumerate them via an **exhaustive `match`** over a
    `RejectReason` / `Flag` value inside the test (the compiler's non-exhaustive-match error
    is itself the tripwire: adding a variant without updating this match fails to compile —
    this is the real enforcement of "a new reason cannot land silently").
  - Assert every code is present in `COVERAGE` **or** in `const DEFERRED: &[(&str, &str /*reason*/)]`.
  - Assert `duplicate` is covered as an *ignored* outcome (it is not a `RejectReason` nor a
    `Flag`; register it as a literal string `"duplicate"`).
- `DEFERRED` starts **empty**. The taxonomy is fully coverable by this suite; the mechanism
  exists so that any future reason added out of band is a compile-or-test failure, forcing a
  conscious "cover it or justify deferral" decision. The one candidate for a *pointer rather
  than a re-test* is the **segregated-admin-fork** dimension of `equivocation` (`§0`/`§12`),
  whose detection lives in the sync layer (`SyncEngine`); `equivocation` itself is covered by
  `§12` at the fold layer, so this stays covered, with a doc note pointing at
  `sync_convergence.rs` for the admin-tip path.

### 4.5 Assertion API per layer

| Vector class | Entry point under test | Assertion shape |
|---|---|---|
| `§1`,`§3` serialization | CSB encoder + `event_id` derivation | byte-exact `Vec<u8>` / hex string equality |
| `§2`,`§4`–`§7`,`§14`-form rejections | `validate_wire_bytes(wire, &ctx)` | `Err(RejectReason::X)` |
| `§8` duplicate | `EventStore::insert` twice / fold ingest twice | `InsertOutcome::Duplicate`; snapshot unchanged |
| `§9` out-of-order | `RoomMembership::ingest` child-before-parent | `Ingest::Buffered { missing }` then accept on parent arrival; final snapshot == in-order |
| `§10` total order | derived `(lamport, event_id)` sort | pinned timeline vector; tie `E_join_dave < E_kick_dave` |
| `§11`,`§13`,`§14`,`§15`,`§18`,`§19` | `validate_with_membership` / `RoomMembership` fold | reason code or resolved snapshot (`status`/`role`) |
| `§16` blob gate | `membership::access::blob_serve_allowed` | per-dialer accept/reject bool |
| `§17` pipe gate | `membership::access::pipe_connect_allowed` | per-dialer accept/reject bool |
| `§12` equivocation | `RoomMembership` fold flags | `Flag::Equivocation` raised, both events kept, ids distinct |
| `§20` clock skew | `validate_wire_bytes` with two `ValidationContext.now` | identical `ValidatedEvent`; skewed one carries `Flag::ClockSkew`, both in set |

---

## 5. Implementation steps (ordered, executable)

1. **Create the binary + module skeleton** (§4.1). Empty submodules with `#[allow(unused)]`
   compile first; wire `protocol_conformance.rs`.
2. **`fixtures.rs` — Cast + room (Tier 1).** Reproduce all five principals from seeds; assert
   the 10 public keys against the spike hex table; derive and assert `room_id_A`/`room_id_B`.
   Port the existing `arr32`/`arr16`/seed helpers from `golden_vectors.rs`.
3. **`serialization.rs` — vectors §1–§7.** Port and rename the corresponding existing
   assertions from `golden_vectors.rs` into §-named tests, filling any missing `THEN` clause
   (e.g. `§2` must cover all five non-canonical sub-cases a–e; `§7` must assert both the
   `room_id_mismatch` reject **and** the re-signed id `81b6a82b…f057`). Keep `golden_vectors.rs`
   in place for now; §8 covers deduplication.
4. **`fixtures.rs` — fixture-log DAG (Tier 2).** Build `E_create … E_pipe` via the real
   builders / local assemblers; generate and pin each `event_id` per §4.3; add the divergence
   doc comment where a produced id differs from the spike placeholder. Expose the validated
   set and per-handle accessors.
5. **`idempotency_ordering.rs` — §8–§10.**
   - `§8`: ingest `E_msg_bob` twice (fold) and insert twice (store, behind the `store`
     feature); assert `Duplicate`, snapshot/timeline byte-identical, and 1× ≡ 1000× via a loop.
   - `§9`: deliver `E_file` before `E_msg_bob`; assert `Ingest::Buffered` with the missing
     parent, then acceptance and derived-lamport `6` after the parent arrives; final snapshot
     equals in-order delivery.
   - `§10`: assemble the validated set incl. the `lamport 7` siblings; sort by
     `(lamport, event_id)`; assert the pinned timeline and the tie order
     `E_join_dave (53c561a9…) < E_kick_dave (b4f8a960…)`.
6. **`membership.rs` — §11–§19.** One test each:
   - `§11` concurrent join-vs-kick → Dave `Removed` on both arrival orders (feed P1 A-then-B,
     P2 B-then-A; assert identical snapshot).
   - `§13` Mallory `message.text` → `NotAMember`.
   - `§14` Bob-signed `member.invited`/`member.removed` → `InsufficientRole`; Bob-signed
     `member.left{member_id=bob_id}` → **accepted** (self-leave positive control).
   - `§15` bad secret → `BadCapability`; consumed/expired invite → `ExpiredInvite`.
   - `§16` `blob_serve_allowed`: Carol accept, Dave reject, Mallory reject.
   - `§17` `pipe_connect_allowed`: Alice accept, Carol reject (not in `allowed_members`),
     Dave reject (Removed), Mallory reject (non-member).
   - `§18` two concurrent `member.invited` with conflicting `role` → resolved `agent`
     (least-privilege), lowest-`event_id` tie-break; pin the produced ids.
   - `§19` rejoin citing a pre-`member.left` invite → `ExpiredInvite`.
   - (Also add `unbound_device` → `UnboundDevice` here — a `member.joined` whose device
     binding does not match the identity key; needed for taxonomy completeness.)
7. **`advisory.rs` — §20 + discipline.**
   - `§20`: validate the same skewed `member.joined` under `ctx.now` where it is (a) >300 s
     ahead and (b) in-bounds; assert **identical** `ValidatedEvent` bytes/id in both, and that
     (a) carries `Flag::ClockSkew` while (b) does not — and that the event is in the validated
     set either way (never dropped, reordered, or authorization-changed).
   - Port the threshold tests (`exactly_at_threshold` not flagged, `+1ms` flagged) from
     `golden_vectors.rs`.
   - `equivocation` (`§12`): Alice authors `E_eq_a`/`E_eq_b` on the same parents without
     self-parenting; assert `Flag::Equivocation` (CRITICAL for admin), both kept, ids distinct,
     snapshot deterministic.
8. **`taxonomy.rs` — coverage matrix + gate (§4.4).** Implement `COVERAGE`, `DEFERRED`
   (empty), the exhaustive-match enumerators for `RejectReason` and `Flag`, and
   `every_reason_and_flag_is_covered_or_deferred`. Add `reason_and_flag_code_strings_match_spec`
   asserting **all 14** reason codes and **all 3** flag codes (closes the 9-of-14 gap noted
   in §3).
9. **Traceability doc comment** in `conformance/mod.rs`: the full §1–§20 → test-fn table and
   the §8 code → vector map (mirror of §6/§7 below).
10. **De-duplicate.** Once the new suite is green and covers a superset of the ported
    stateless assertions, delete the now-redundant fns from `golden_vectors.rs` **or** (safer,
    recommended) leave `golden_vectors.rs` intact and mark the new binary as the authoritative
    conformance suite in the doc comment, accepting minor overlap. Decide per §8 Open
    Questions — default: **keep both**, overlap is cheap, deletion risks losing an assertion.
11. **Run `scripts/verify.sh`** locally; confirm the new binary appears in `cargo test`
    output and CI (`.github/workflows/verify.yml` already runs `scripts/verify.sh` with
    `--all-features`, so the `store`-gated `§8` store assertion runs in CI).

---

## 6. Vector → test map (all 20)

| Vector | Test fn (`conformance::*`) | Module | Primary assertion |
|---|---|---|---|
| §1 canonical determinism | `vector_01_canonical_serialization_determinism` | serialization | scrambled-key build ⇒ identical 242-byte CSB |
| §2 non-canonical rejected | `vector_02_non_canonical_encoding_rejected` | serialization | 5 sub-cases ⇒ `NonCanonicalEncoding` |
| §3 event_id derivation | `vector_03_event_id_is_recomputed` | serialization | `id == blake3:c389e2…85a1`; doctored id ⇒ `IdMismatch` |
| §4 room_id bound (genesis) | `vector_04_room_id_derivation_bound` | serialization | derive `43c19f2e…16a3`; forged ⇒ `RoomIdMismatch` |
| §5 signature accept/reject | `vector_05_signature_under_device_key` | serialization | verify under `device_id` ok; under `sender_id` ⇒ `BadSignature` |
| §6 tamper ⇒ id+sig fail | `vector_06_tampered_field_breaks_id_and_signature` | serialization | id ⇒ `6267b72c…c75c`; `IdMismatch` + `BadSignature` |
| §7 cross-room replay | `vector_07_cross_room_replay_rejected` | serialization | `RoomIdMismatch`; re-signed id `81b6a82b…f057` |
| §8 duplicate idempotency | `vector_08_duplicate_ignored_idempotently` | idempotency_ordering | `Duplicate`; 1×≡1000×; state unchanged |
| §9 out-of-order buffering | `vector_09_child_before_parent_buffered` | idempotency_ordering | `Buffered{missing}` then accept; == in-order |
| §10 total order | `vector_10_deterministic_total_order` | idempotency_ordering | pinned timeline; `join < kick` at lamport 7 |
| §11 concurrent join/kick | `vector_11_concurrent_join_kick_removed` | membership | Dave `Removed` both orders |
| §12 equivocation | `vector_12_admin_equivocation_flagged` | advisory | `Flag::Equivocation` CRITICAL; both kept |
| §13 non-member rejected | `vector_13_non_member_event_rejected` | membership | `NotAMember` |
| §14 insufficient role | `vector_14_insufficient_role_rejected` | membership | `InsufficientRole`; self-leave accepted |
| §15 stale invite / bad cap | `vector_15_bad_capability_and_expired_invite` | membership | `BadCapability`; `ExpiredInvite` |
| §16 blob serve gate | `vector_16_blob_serve_gate` | membership | Carol✓ Dave✗ Mallory✗ |
| §17 pipe connect gate | `vector_17_pipe_connect_gate` | membership | Alice✓ Carol✗ Dave✗ Mallory✗ |
| §18 concurrent attributes | `vector_18_concurrent_attributes_least_privilege` | membership | role⇒`agent`; lowest-id tie-break |
| §19 leave then rejoin | `vector_19_leave_consumes_invite` | membership | `ExpiredInvite` |
| §20 clock skew advisory | `vector_20_clock_skew_advisory_only` | advisory | same set; `ClockSkew` flag only |

---

## 7. Taxonomy coverage matrix (§8) — every outcome mapped

**Rejections (14):**

| Code | Covered by |
|---|---|
| `unknown_schema_version` | serialization (ported: `unknown_schema_version_is_rejected`) |
| `unknown_event_type` | serialization (ported) |
| `non_canonical_encoding` | §2 |
| `id_mismatch` | §3, §6 |
| `bad_signature` | §5, §6 |
| `unbound_device` | §6-adjacent: `vector_14`-area `UnboundDevice` case (step 6) |
| `not_a_member` | §13 |
| `insufficient_role` | §14 |
| `room_id_mismatch` | §4, §7 |
| `invalid_content` | serialization (ported content-bounds cases: body-too-long, bad enum, pct>100, empty admins, …) |
| `expired_invite` | §15, §19 |
| `bad_capability` | §15 |
| `too_many_parents` | serialization (ported: `too_many_parents_is_rejected`, >20) |
| `not_genesis_descended` | serialization (ported: empty/floating `prev_events`) |

**Ignored (1):** `duplicate` → §8.

**Advisory flags (3):** `clock_skew` → §20; `equivocation` → §12 (+ admin-fork pointer to
`sync_convergence.rs`); `from_removed_member` → membership (ported/added: a removed member's
log-valid event carries `Flag::FromRemovedMember`; assert in `membership.rs`).

`DEFERRED` = ∅. The `every_reason_and_flag_is_covered_or_deferred` gate enforces this table
stays complete.

---

## 8. Key decisions

1. **New §-indexed binary, not a refactor of `golden_vectors.rs`.** Traceability and the
   completeness gate need a clean, purpose-built structure; the 57 existing test fns stay green
   and untouched. Overlap between the two is accepted as cheap insurance (see step 10).
2. **Regenerate Tier-2 fixture-log ids from the implementation and pin them**, cross-checking
   only the Tier-1 independently-reproduced values against the spike. This follows the spike's
   explicit instruction and turns the un-reproduced placeholders into locked regression golden.
3. **Assert transport-layer vectors (§16/§17) at the pure access-predicate layer**
   (`blob_serve_allowed` / `pipe_connect_allowed`), not over live QUIC. Keeps the suite
   deterministic, fast, and network-free; the QUIC e2e stays in `iroh-rooms-net`.
4. **Enum exhaustiveness as the enforcement primitive.** The completeness gate leans on the
   compiler's non-exhaustive-`match` error so a new `RejectReason`/`Flag` *cannot* be added in
   `src/` without breaking this test — stronger than a runtime string list alone.
5. **Local fixture assemblers for builder-less event types** (`member.removed`, `member.left`,
   `file.shared`, `agent.status`) instead of adding production builders — this is a test-only
   PR (non-goal §2).

---

## 9. Risks and mitigations

| Risk | Sev | Mitigation |
|---|---|---|
| A regenerated Tier-2 id reveals the landed schema diverges from a spike **Tier-1** value (a real encoder bug surfacing via the fixture log). | High | Tier-1 values are asserted independently and first; any Tier-1 mismatch is a hard NO-GO filed as a **blocker issue**, not a golden update. |
| A vector can't be expressed because a production predicate is missing/misnamed (e.g. no public `pipe_connect_allowed` shape for the exact `§17` inputs). | Med | Filed as a separate `area/protocol` issue; the vector is marked `#[ignore]` with a `// BLOCKED on #NNN` note rather than weakened — surfaces the gap without a fake pass. |
| Silent partial coverage — a `THEN` clause omitted so a vector "passes" while under-asserting. | Med | Each vector's doc comment quotes its `THEN`; review checklist requires every clause to have an assertion; the coverage matrix is code-reviewed against §7. |
| Duplicate/store vector needs the `store` cargo feature; a default-feature `cargo test` skips it. | Low | CI runs `--all-features` (verified in `verify.yml`); gate the store half behind `#[cfg(feature = "store")]` and also assert dup-idempotency at the **fold** layer (feature-independent) so the core property is always exercised. |
| Overlap/drift between `golden_vectors.rs` and the new suite. | Low | Doc comment names the new binary authoritative; overlap tolerated; no deletion in this PR (Open Q1). |
| Non-determinism sneaks in (map iteration order, clock reads). | Med | Fixtures are seed-derived and clock-injected (`ValidationContext.now`, `created_at` are explicit inputs); no wall-clock reads in any vector. |

---

## 10. Acceptance criteria (from the issue, mapped)

- [ ] **Golden CSB, event ID, signature, and room ID reproduce exactly.** → §1/§3/§4/§5 in
      `serialization.rs`; Tier-1 constants asserted in `fixtures.rs`.
- [ ] **Non-canonical encodings are rejected.** → §2 (all five sub-cases a–e) ⇒
      `NonCanonicalEncoding`.
- [ ] **Clock skew is accepted with advisory flag, not rejected.** → §20 + threshold tests in
      `advisory.rs`: same validated set, `Flag::ClockSkew` only.
- [ ] **Duplicate events are ignored idempotently.** → §8: `Duplicate`, 1×≡1000×, state
      unchanged, at both fold and store layers.
- [ ] **Every rejection/flag reason in the spike taxonomy is covered or explicitly deferred.**
      → `taxonomy.rs`: exhaustive-match enumeration + `COVERAGE`/`DEFERRED` gate; `DEFERRED` is
      empty; §7 matrix maps all 14 + 1 + 3 outcomes.

**Definition of done:** `scripts/verify.sh` passes locally and in CI; `cargo test -p
iroh-rooms-core --test protocol_conformance --all-features` runs one `#[test]` per vector §1–§20
plus the completeness gate, all green; the traceability table in `conformance/mod.rs` matches
§6/§7.

---

## 11. Test plan

- **Local:** `cargo test -p iroh-rooms-core --test protocol_conformance --all-features`
  (fast, network-free, deterministic). Full gate: `scripts/verify.sh`.
- **CI:** already covered — `.github/workflows/verify.yml` runs `scripts/verify.sh` on every
  PR and on `main`, which invokes `cargo test --workspace --all-targets --all-features`,
  picking up the new binary and its `store`-gated half.
- **Regression semantics:** any change to the CBOR codec, an event content schema, a builder,
  or a derivation that alters bytes will break a pinned golden id — the suite is the
  byte-level tripwire the spike's Gate B/Gate D require.

---

## 12. Open questions

1. **Delete overlapping fns from `golden_vectors.rs`, or keep both?** Default: **keep both**
   (overlap is cheap; deletion risks silently dropping an assertion). Revisit if the
   duplication becomes a maintenance cost.
2. **Should `agent.status` get a conformance vector?** The spike has no numbered vector for it
   (it is "an ordinary event type"), but `invalid_content` for `agent.status` (`progress_pct
   > 100`) is a taxonomy case already covered. Proposal: cover it under `invalid_content` only;
   no dedicated vector. Confirm at review.
3. **`from_removed_member` exact assertion surface.** It is a current-snapshot attribution flag
   (`fold.rs`), not a validate-time flag. Confirm the fold exposes it on the ingest/snapshot
   path the vector can read; if not, file as a §9-style blocked vector.
4. **Property/fuzz follow-up.** Out of scope here; propose a follow-up issue to fuzz the CBOR
   canonicaliser against `validate_wire_bytes` (differential: decode→re-canonicalise→byte-eq).

## 13. Assumptions

- The landed `iroh-rooms-core` public API (`validate_wire_bytes`, `validate_with_membership`,
  `RoomMembership::{ingest,snapshot,ancestor_view}`, `membership::access::{blob_serve_allowed,
  pipe_connect_allowed}`, `EventStore::insert`, the builders, `RejectReason`, `Flag`) is
  stable for the duration of this work; no `src/` changes are needed to express any vector
  (if one is, it becomes a separate issue per §2).
- Tier-1 golden values in the spike are authoritative and already reproduced by the landed
  encoder (confirmed present in `golden_vectors.rs`).
- `--all-features` is and remains the CI test invocation, so the `store`-gated assertions run.
