# Spec: Direct unit + property/fuzz tests for the strict CBOR reader (risk R1)

| | |
|---|---|
| **Issue** | #45 — [IR-0002] Add direct unit + property/fuzz tests for strict CBOR reader (risk R1) |
| **Parent / origin** | #6 / PR #44 (merge `df41ec2`) — event-core canonical signed event model. Filed as a follow-up by the ADW review phase (run 62e49d66). |
| **Labels** | type/test, area/protocol, risk/high (test-coverage of the spec's #1 risk) |
| **Traceability** | `specs/event-core-canonical-signed-event-model.md` §10.3, §11.4, risk **R1** · `PHASE-0-SPIKE.md` Event Protocol §3 (deterministic-CBOR profile) |
| **Status** | Proposed (planning only — no code in this task) |
| **Type** | **Tests + tooling only.** New `#[cfg(test)]` module in `cbor.rs`, a new integration test file, one dev-dependency, and an optional non-gating fuzz crate. **No production code changes.** |

---

## 1. Summary

The strict deterministic-CBOR reader in `crates/iroh-rooms-core/src/event/cbor.rs` is the spec's
headline risk **R1** (malformed / adversarial CBOR on the signed-event trust boundary), yet it has
**zero direct tests**. Its `decode_canonical`, `encode`, and the fourteen `CborError` rejection
variants are exercised only *indirectly* through `validate.rs` / `wire.rs`, where every `CborError`
is collapsed to a single `RejectReason::NonCanonicalEncoding` (see `wire.rs:83`,
`validate.rs:97`). The indirect tests therefore cannot tell which rejection path actually fired,
cannot pin the accept boundaries, and cannot prove the "never panics / never over-allocates on
hostile input" guarantee that `cbor.rs:196-198` and spec §10.3 promise.

This issue closes both gaps flagged (but not blocked) by the #6 review:

1. **Direct unit tests for `cbor.rs`** — one crafted input per rejection path asserting the exact
   `CborError` variant, plus accept-path and boundary cases (shortest-form, depth, canonical map
   ordering, preallocation guard).
2. **Property / fuzz tests** (spec §10.3 / §11.4) — a `proptest`-driven robustness suite proving
   *no panic + typed result* over arbitrary bytes for both `decode_canonical` and the full
   `validate_wire_bytes` pipeline, plus the canonicalization round-trip invariant, plus an
   **optional** `cargo-fuzz` target (documented, not part of the CI gate).

The header comment at `crates/iroh-rooms-core/tests/golden_vectors.rs:8` explicitly acknowledges
these are "added separately"; this issue is that separate work. **This is test/tooling work only —
no production behavior changes.**

---

## 2. Background & current repository state

Read before implementing:

- **`crates/iroh-rooms-core/src/event/cbor.rs`** — the module under test (407 lines):
  - `pub fn decode_canonical(&[u8]) -> Result<CborValue, CborError>` — decode exactly one
    canonical item, requiring the whole input is consumed (`TrailingData` otherwise).
  - `pub fn encode(&CborValue) -> Vec<u8>` — canonical encoder (sorts map keys length-first then
    bytewise; shortest-form heads).
  - `pub enum CborValue { Uint, Bytes, Text, Array, Map }` — the closed profile.
  - `pub enum CborError` — **14 variants** (see §5 matrix). Every variant is `pub`.
  - Private internals: `struct Reader`, `Reader::{take, read_u8, read_head, checked_len,
    read_value}`, `write_head`, `encoded_key`, `len_arg`, `const MAX_DEPTH: usize = 16`.
- **`crates/iroh-rooms-core/src/event/validate.rs` / `wire.rs`** — the only current call sites.
  Both map any `CborError` to `RejectReason::NonCanonicalEncoding` via `.map_err(|_| …)`, which is
  *why* direct unit tests are needed to distinguish the paths.
- **`crates/iroh-rooms-core/tests/golden_vectors.rs`** and
  **`tests/conformance/serialization.rs`** — existing conformance tests. They already craft raw
  non-canonical CBOR (see `serialization.rs:84-190`, `noncanonical_a..e`) but assert only the
  collapsed `NonCanonicalEncoding` outcome through the pipeline, not the underlying `CborError`.
- **Inline-test convention** — the crate already uses `#[cfg(test)] mod tests { … }` at the bottom
  of module files for module-focused unit tests (e.g. `content.rs:975-1026`, plus `keys.rs`,
  `signed.rs`, `message.rs`, …). `cbor.rs` is the notable module *without* one.
- **`crates/iroh-rooms-core/Cargo.toml`** — `[dev-dependencies]` currently holds only `tempfile`.
  No `proptest` / `arbitrary` / `quickcheck` anywhere in the workspace.
- **`scripts/verify.sh`** — the real CI gate (matches the auto-memory note "verify.sh is the real
  CI gate"):
  ```bash
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-targets --all-features
  ```
  Two consequences that shape this issue:
  - **Clippy pedantic is effectively deny in test code too** (`--all-targets`), so `proptest!`
    macro bodies and generated closures must be pedantic-clean (targeted `#[allow]` if needed).
  - **`cargo test` runs on stable and only sees workspace members.** A `cargo-fuzz` target is
    nightly + libfuzzer and must live in a **workspace-excluded** crate so it never enters the
    gate.

**Critical current-state facts:**

1. `cbor.rs` has no `#[cfg(test)]` module — the direct unit tests are net-new and have a natural,
   convention-matching home inside the file.
2. Every `CborError` is publicly reachable through `decode_canonical`, so unit tests can assert the
   exact variant without touching internals. (Access to private helpers is *not required*; keeping
   the tests on the public surface keeps them robust to refactors.)
3. `MAX_DEPTH = 16` and the numeric caps (`MAX_PREV_EVENTS`, `MAX_MESSAGE_BODY_BYTES`, …) are the
   boundaries to pin; import them from `constants.rs` rather than hard-coding.
4. The encoder is the inverse oracle: for any input `decode_canonical` accepts, `encode(value)`
   MUST reproduce the input byte-for-byte (`cbor.rs:16-19` — "decode → re-encode → byte-equal").
   This is the core R1 invariant and the anchor of the round-trip property test.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. **Direct unit tests** for `cbor.rs` (`#[cfg(test)] mod tests`) covering:
   - one crafted input per `CborError` variant, asserting the **exact** variant (§5.1);
   - accept-path cases for all five `CborValue` kinds (§5.2);
   - boundary cases: shortest-form int/length limits, canonical map key ordering (length-first),
     nesting depth at `MAX_DEPTH` (accept) and `MAX_DEPTH+1` (reject), preallocation guard on an
     oversized declared length (§5.2/§5.3);
   - encoder/decoder round-trip and idempotence on canonical inputs (§5.2).
2. **Property tests** (`proptest`) in a new integration test file (§6):
   - **P1 — robustness:** arbitrary `Vec<u8>` → `decode_canonical` returns `Ok | Err(CborError)`
     and never panics; same for `validate_wire_bytes`.
   - **P2 — canonical round-trip:** for arbitrary generated `CborValue` (bounded), `encode` output
     re-decodes to the canonicalized value and re-encodes identically (`canonical(encode(x)) ==
     encode(x)`, `decode(encode(x)) == canonical(x)`).
   - **P3 — encoder ⊆ strict reader:** every `encode(value)` is accepted by `decode_canonical`
     (the encoder can never emit non-canonical bytes).
   - **P4 — mutation stays typed:** single-byte mutations of a valid `WireEvent` keep
     `validate_wire_bytes` panic-free and typed (and, for the provable classes, `Err`).
3. **Optional fuzz target** (`cargo-fuzz`, §7): a workspace-excluded `fuzz/` crate with a
   `cbor_decode` target (idempotence oracle) and a `validate_wire_bytes` target, plus a seed
   corpus and run instructions. **Explicitly not wired into `verify.sh`.**
4. Add `proptest` to `[dev-dependencies]` of `iroh-rooms-core`.

### 3.2 Non-goals

- **No production code changes.** If a test surfaces a genuine defect in `cbor.rs`, file a separate
  bug issue; do not fix it under this test-only issue (record the finding, add a `#[ignore]`'d
  reproducer or a comment, and escalate).
- No changes to `validate.rs` / `wire.rs` behavior or the `RejectReason` collapse (the collapse is
  intentional per spec §8 — every non-canonical form is one wire reason code).
- No new public API on `cbor.rs`. Tests use the existing public surface.
- No fuzzing *in CI* (nightly + libfuzzer are out of the stable gate's scope). The fuzz crate is a
  developer tool, run on demand.

### 3.3 Why this is safe and self-contained

The reader is a pure function of its input bytes; the tests need no network, store, or membership
state. Adding tests + one dev-dependency + an excluded fuzz crate cannot change any shipping
behavior, and `verify.sh` proves the additions are fmt/clippy/test-clean.

---

## 4. Key decisions

### D1 — Direct unit tests live **inline** in `cbor.rs` (`#[cfg(test)] mod tests`)

Rationale: the issue says "direct unit tests for `cbor.rs`"; the crate convention for
module-focused unit tests is an inline `#[cfg(test)] mod tests` (content.rs, keys.rs, …); and
inline tests keep the rejection-path fixtures next to the code they pin and retain the *option* to
reach private helpers (`checked_len`, `write_head`) if a future case needs it. The public surface
(`decode_canonical`, `encode`, `CborValue`, `CborError`) is sufficient for every case in §5, so the
tests stay on the public API by default.

*Alternative considered:* a `tests/cbor_reader.rs` integration file. Rejected as the primary home
because it can only see the public surface (no real loss here) and diverges from the crate's
unit-test convention — but it is an acceptable fallback if reviewers prefer to keep `cbor.rs`
shorter. Pick one; do not split the unit cases across both.

### D2 — `proptest` is the property engine; property tests live in a new integration file

Rationale: `proptest` is the engine spec §11.4 names first, has a shrinker (minimal failing input
on regression — exactly what you want for an adversarial-parser bug), and a recursive strategy for
generating bounded `CborValue`s is straightforward. Put the property suite in a **new integration
test** `crates/iroh-rooms-core/tests/cbor_property.rs` so the `proptest` dependency stays a
dev-dependency scoped to a single target and the inline unit module stays fast and dependency-free.

- **Version:** pin `proptest = "1"` in `[dev-dependencies]`. It is a stable, widely-used 1.x line.
- **Regression persistence:** commit the `proptest-regressions/` files proptest writes on a found
  failure (they pin the failing seed so CI re-checks it), OR set
  `ProptestConfig { failure_persistence: None, .. }` if the team prefers no committed artifacts.
  **Recommend committing** the regression files (default behavior) — see Open Q2.
- **Case budget:** keep strategies small (byte vecs `0..256` bytes, `CborValue` depth `≤ 8`,
  collection sizes `0..8`) and, for the `validate_wire_bytes` property (which runs Ed25519 verify),
  cap `ProptestConfig::cases` (e.g. 128) so the suite stays well within a normal `cargo test` run.

### D3 — The `cargo-fuzz` target is **optional and excluded from the workspace / CI**

`cargo-fuzz` needs nightly + libfuzzer; `verify.sh` runs stable `cargo test` over workspace
members. Putting the fuzz crate in `[workspace].members` would force a nightly-only build into CI.
Therefore the `fuzz/` crate is a standalone crate **not** listed in the root `[workspace].members`
(cargo-fuzz's default layout already keeps it separate via its own `Cargo.toml` with
`[workspace]` empty). Document how to run it in the module docs / a short `fuzz/README.md`; it is a
developer tool, not a gate. Mark it clearly optional so the issue can land the unit + property
tests even if the fuzz target is deferred (spec §11.4: "Optional `cargo-fuzz` target … if time
allows").

### D4 — Fixtures must respect the reader's **error precedence**

`read_head` (`cbor.rs:231-276`) enforces shortest-form integer encoding for **every** item head —
including the length fields of byte/text/array/map and the payload of major-type 6/7 items — *before*
the major-type dispatch in `read_value`. Consequence: a crafted "float" or "tag" that uses the
25/26/27 head forms can surface `NonShortestInt` instead of `FloatOrSimple` / `Tag` depending on its
payload bytes. Tests MUST pick head bytes that isolate the intended variant:

- For `FloatOrSimple`, use simple values `0xf4`/`0xf5`/`0xf6`/`0xf7` (info 20–23) or `0xf8 0xff`
  (info 24, value > 23) — **not** `0xf9/0xfa/0xfb` (float16/32/64), whose payload may trip the
  shortest-form check first.
- For `Tag`, use `0xc0` (tag 0, info 0) — the major-6 dispatch fires immediately, before any
  payload is read.

This precedence is itself worth pinning: a dedicated case asserts a non-shortest **length** head
(e.g. `0x58 0x17` = a byte string whose length 23 is written in the 1+1 form) yields
`NonShortestInt`, proving shortest-form is enforced on lengths, not just integer values.

### D5 — Assert exact variants, import caps from `constants.rs`

Unit tests assert `Err(CborError::Foo)` precisely (not just `is_err()`), and pull `MAX_DEPTH` (via a
test-visible path — see Open Q3), `MAX_PREV_EVENTS`, `MAX_MESSAGE_BODY_BYTES` from `constants.rs`
rather than hard-coding, so a future cap change updates one place.

---

## 5. Unit-test plan (`cbor.rs` `#[cfg(test)] mod tests`)

All inputs below are hex byte strings fed to `cbor::decode_canonical`. `⇒` denotes the asserted
result. Values are chosen minimal and single-defect.

### 5.1 Rejection paths — one case per `CborError` variant (14)

| # | Variant | Input (hex) | Notes |
|---|---|---|---|
| R-01 | `UnexpectedEof` | `43 01 02` | `bstr(3)` declares 3 bytes, only 2 follow → `checked_len`/`take` short. Also add a truncated-head case `18` (24-form missing its second byte). |
| R-02 | `TrailingData` | `00 00` | `Uint(0)` decodes; one byte remains. Also `a0 00` (empty map + extra). |
| R-03 | `IndefiniteLength` | `1f` | major-0 info-31. Add the container forms `5f`, `7f`, `9f`, `bf` (indefinite bstr/tstr/array/map) — all ⇒ `IndefiniteLength`. |
| R-04 | `NonShortestInt` | `18 17` | 24-form encoding of 23 (`v ≤ 23`). Add `19 00 ff` (255 in 2-byte), `1a 00 00 ff ff` (65535 in 4-byte), `1b 00 00 00 00 ff ff ff ff` (2³²−1 in 8-byte), and the **length-field** case `58 17`. |
| R-05 | `NegativeInteger` | `20` | major-1 (−1). Add `38 63` (−100). |
| R-06 | `ReservedAdditionalInfo` | `1c` | major-0 info-28. Add `1d`, `1e`. |
| R-07 | `Tag` | `c0` | major-6, info-0 (tag 0) — dispatches before payload (see D4). |
| R-08 | `FloatOrSimple` | `f5` | `true` (major-7 info-21). Add `f4`, `f6`, `f8 ff` (see D4 — avoid `f9/fa/fb`). |
| R-09 | `NonTextMapKey` | `a1 00 00` | `map(1)` with a `Uint` key. Add `a1 41 61 00` (bstr key). |
| R-10 | `UnsortedMapKey` | `a2 61 62 00 61 61 00` | `map(2)` keys `"b"` then `"a"` (descending). Add the length-first case `a2 62 61 61 00 61 62 00` (`"aa"` before `"b"` — longer key first is out of order). |
| R-11 | `DuplicateMapKey` | `a2 61 61 00 61 61 00` | `map(2)` key `"a"` twice. |
| R-12 | `InvalidUtf8` | `62 ff ff` | `tstr(2)` with non-UTF-8 bytes. Add the map-key form `a1 62 ff ff 00`. |
| R-13 | `DepthExceeded` | `81`×17 then `00` | 17 nested single-element arrays; the innermost element is read at depth 17 (`MAX_DEPTH = 16`) → reject. |
| R-14 | `LengthOverflow` | `bb 80 00 00 00 00 00 00 00` | `map(*)` count 2⁶³; `checked_len(arg, 2)` overflows `n * 2` → `LengthOverflow` on 64-bit. (On 32-bit the `usize::try_from(arg)` branch yields the same variant.) **No allocation occurs** — assert it returns quickly, proving the preallocation guard. |

> Notes for the author: `UnexpectedEof` also covers the **oversized declared length** anti-DoS path —
> add `58 20` followed by only 5 payload bytes (`bstr(32)` with 5 present): `checked_len` sees
> `needed(32) > remaining(5)` and returns `UnexpectedEof` **without** allocating 32 bytes. This is
> the concrete R1/DoS guard and should be an explicit, commented case even though it shares the
> `UnexpectedEof` variant with R-01.

### 5.2 Accept paths + round-trip

For each, assert `decode_canonical` returns the expected `CborValue`, and that the round-trip holds
both directions:

- `encode(&decode_canonical(bytes)?) == bytes` (idempotence on canonical input), and
- `decode_canonical(&encode(&value))? == value`.

| # | Kind | Input (hex) | Decodes to |
|---|---|---|---|
| A-01 | `Uint` boundaries | `00`, `17`, `18 18`, `18 ff`, `19 01 00`, `19 ff ff`, `1a 00 01 00 00`, `1b 00 00 00 01 00 00 00 00` | `Uint(0/23/24/255/256/65535/65536/2³²)` — the minimal shortest form at each width boundary. |
| A-02 | `Bytes` | `40`, `43 01 02 03` | `Bytes([])`, `Bytes([1,2,3])` |
| A-03 | `Text` | `60`, `65 68 65 6c 6c 6f` | `Text("")`, `Text("hello")` |
| A-04 | `Array` | `80`, `82 01 02` | `Array([])`, `Array([Uint(1),Uint(2)])` |
| A-05 | `Map` | `a0`, `a1 61 61 01`, `a2 61 61 01 61 62 02` | `Map([])`, `Map([("a",1)])`, `Map([("a",1),("b",2)])` (ascending) |
| A-06 | Nested depth = `MAX_DEPTH` | `81`×16 then `00` | 16 nested arrays with a leaf read at depth 16 — **accepted** (the boundary counterpart to R-13). |

### 5.3 Canonical-ordering & scrambled-map encoder tests (encoder side)

- Build `CborValue::Map` with keys inserted out of canonical order and assert `encode` emits them
  in canonical length-first-then-bytewise order (mirror of the existing
  `serialization.rs::vector_01` assertion but at the `cbor` unit level).
- Assert `encode` of a map with a key needing the length-first tiebreak (`"z"` len-1 vs `"aa"`
  len-2) orders the shorter key first.
- Assert `encode(decode_canonical(b)?) == b` for the golden 242-byte CSB prefix bytes as an extra
  anchor (optional — the golden CSB is already pinned in `golden_vectors.rs`).

---

## 6. Property-test plan (`tests/cbor_property.rs`, `proptest`)

Add a recursive `CborValue` strategy (bounded): leaves = `Uint(any u64)`, `Bytes(vec 0..8)`,
`Text(valid utf-8 0..8 chars)`; recursion = `Array(0..8 of inner)` and `Map` built from a
`BTreeMap<String, inner>` (unique, so re-encoding is a clean round-trip) with depth `≤ 8`.

- **P1 — robustness / no panic (the §10.3 guarantee).**
  `proptest!(|(bytes in prop::collection::vec(any::<u8>(), 0..256))| { let _ =
  cbor::decode_canonical(&bytes); })` — must complete for every input; a panic (OOB index, unbounded
  alloc, arithmetic overflow) fails the test. Repeat feeding the same random bytes to
  `validate_wire_bytes(&bytes, &ctx)` (must return `Ok | Err(RejectReason)`, never panic). Cap
  `cases` for the pipeline variant (Ed25519 verify per case).

- **P2 — canonical round-trip (the R1 invariant).**
  For `value in cbor_value_strategy()`: `let b = cbor::encode(&value); assert_eq!(cbor::encode(
  &cbor::decode_canonical(&b).unwrap()), b);` (`canonical(encode(x)) == encode(x)`) and
  `assert_eq!(cbor::decode_canonical(&b).unwrap(), canonicalize(value))` where `canonicalize` sorts
  map keys (the generator already yields unique, so this is a key-order normalization).

- **P3 — encoder ⊆ strict reader.**
  For `value in cbor_value_strategy()`: `cbor::decode_canonical(&cbor::encode(&value)).is_ok()` —
  the encoder can never produce bytes the strict reader rejects.

- **P4 — mutation stays typed.**
  Take a valid signed `WireEvent`'s bytes (reuse a fixture like `golden_vectors.rs::genesis_wire_bytes`
  or `conformance/fixtures.rs`). For `(idx, xor) in (0..len, 1u8..=255)`: flip one byte and assert
  `validate_wire_bytes` returns a typed result without panicking. Additionally assert `Err` for the
  provable classes (a mutation inside the `signed` bstr changes the recomputed id → `IdMismatch`, or
  breaks canonical structure → `NonCanonicalEncoding`); keep the *primary* assertion "no panic +
  typed" so the property is robust.

Determinism note: none of these strategies use wall-clock or ambient randomness beyond proptest's
own seeded RNG, so failures are reproducible from the committed regression seed.

---

## 7. Optional fuzz target (`crates/iroh-rooms-core/fuzz/`, `cargo-fuzz`) — non-gating

Layout (standard `cargo fuzz init` output, **excluded** from the root workspace):

```text
crates/iroh-rooms-core/fuzz/
  Cargo.toml            # its own [workspace] (empty) so it is NOT a root member; depends on
                        # iroh-rooms-core + libfuzzer-sys
  fuzz_targets/
    cbor_decode.rs      # decode_canonical(data); on Ok(v) assert encode(&v) == data (idempotence)
                        # and decode_canonical(&encode(&v)) == Ok(v)
    validate_wire.rs    # validate_wire_bytes(data, &ctx) — must never panic
  README.md             # how to run + what each target asserts
```

- **Targets:** `cbor_decode` (idempotence oracle: any accepted input must re-encode to itself) and
  `validate_wire` (panic-freedom on the full pipeline).
- **Seed corpus:** the golden 242-byte CSB, the five `noncanonical_a..e` encodings from
  `serialization.rs`, and a valid `WireEvent`'s bytes — drop into `fuzz/corpus/<target>/`.
- **Run:** `cargo +nightly fuzz run cbor_decode` (documented in `fuzz/README.md`).
- **Not in CI:** `scripts/verify.sh` is unchanged; the fuzz crate is never built by the stable gate.
  Any crash found is triaged into a `#[test]` reproducer (unit or `proptest` regression) so the
  regression is captured by the stable suite going forward.

---

## 8. Implementation steps

1. **Add the dev-dependency.** In `crates/iroh-rooms-core/Cargo.toml` `[dev-dependencies]`, add
   `proptest = "1"` with a short comment (why: robustness/property tests for the R1 CBOR reader;
   dev-only so validate-only consumers stay lean). Run `cargo build --tests` to resolve.
2. **Inline unit tests.** Append `#[cfg(test)] mod tests { … }` to `cbor.rs` implementing every
   case in §5 (R-01..R-14, A-01..A-06, ordering cases, the oversized-length guard case). Use a small
   `fn hx(s: &str) -> Vec<u8>` helper (or `hex::decode`) and assert exact `CborError` variants.
3. **Property test file.** Create `crates/iroh-rooms-core/tests/cbor_property.rs` with the
   `cbor_value_strategy()` and P1–P4. Add module-level `#![allow(clippy::…)]` only if a specific
   pedantic lint is unavoidable in `proptest!` expansions (prefer fixing over allowing; document any
   allow with the exact lint name and reason).
4. **(Optional) Fuzz crate.** `cd crates/iroh-rooms-core && cargo fuzz init`; write the two targets
   and `README.md`; add the seed corpus; confirm the root workspace does **not** list it in
   `[workspace].members` (and that its own `Cargo.toml` has an empty `[workspace]` so it is isolated).
5. **Run the gate.** `scripts/verify.sh` must be green: `fmt --check`, `clippy --all-targets
   --all-features -D warnings` (pedantic), `cargo test --workspace --all-targets --all-features`.
   The new tests run under `cargo test`; the fuzz crate is excluded and does not.
6. **If a real bug is found** (a panic, an over-allocation, a canonicalization asymmetry): do **not**
   fix `cbor.rs` here. Capture the minimized input, open a separate `type/bug risk/high` issue
   referencing R1, and either `#[ignore]` the reproducer with a link or gate it behind the new bug
   issue. Report it in the PR description.

---

## 9. Risks & mitigations

- **Rk1 — Clippy pedantic on `proptest!`/test code (MEDIUM).** `--all-targets` applies
  `-D warnings` pedantic to tests; macro expansions can trip lints (e.g. `used_underscore_binding`,
  `ignored_unit_patterns`). *Mitigation:* keep closures explicit, prefer per-item `#[allow]` with a
  named lint over a broad module allow, and verify with the exact CI command before finishing.
- **Rk2 — Fuzz crate leaking into the CI gate (MEDIUM).** If the `fuzz/` crate is added to
  `[workspace].members`, stable `cargo build/clippy --workspace` would try to build a nightly-only
  crate and break CI. *Mitigation:* keep it workspace-excluded (D3); `verify.sh` is unchanged;
  add a checklist item to confirm root `Cargo.toml` is untouched.
- **Rk3 — Error-precedence fixtures asserting the wrong variant (MEDIUM).** A "float"/"tag" fixture
  using a 25/26/27 head may yield `NonShortestInt` not `FloatOrSimple`/`Tag` (D4). *Mitigation:*
  the §5.1 table already pins precedence-safe bytes; add a comment on each such case.
- **Rk4 — Slow property suite (LOW).** The `validate_wire_bytes` property runs Ed25519 verify per
  case. *Mitigation:* bound byte-vec length and cap `ProptestConfig::cases` (D2); the
  `decode_canonical`-only properties stay cheap and can keep the default case count.
- **Rk5 — 32-bit vs 64-bit `LengthOverflow` reachability (LOW).** On 64-bit, `LengthOverflow` is
  reached via the `checked_mul` overflow (map count ≥ 2⁶³), not the `usize::try_from` branch.
  *Mitigation:* the R-14 fixture uses the `checked_mul` path (portable), and a comment records that
  the `try_from` branch is the 32-bit-only route to the same variant.
- **Rk6 — A found bug tempts an in-scope fix (LOW/process).** *Mitigation:* §3.2 / step 6 make the
  no-production-change boundary explicit; escalate via a new issue.

---

## 10. Acceptance criteria

- [ ] `cbor.rs` has an inline `#[cfg(test)] mod tests` with **one asserting case per `CborError`
      variant** (all 14), each asserting the exact variant, plus the oversized-declared-length
      preallocation-guard case.
- [ ] Accept-path tests cover all five `CborValue` kinds, the shortest-form width boundaries, the
      canonical map ordering (incl. length-first tiebreak), and nesting depth at `MAX_DEPTH` (accept)
      / `MAX_DEPTH+1` (reject).
- [ ] Round-trip/idempotence assertions hold: `encode(decode(b)) == b` for accepted `b`, and
      `decode(encode(v)) == canonicalize(v)`.
- [ ] `tests/cbor_property.rs` implements P1 (no-panic/typed over random bytes for both
      `decode_canonical` and `validate_wire_bytes`), P2 (canonical round-trip), P3 (encoder ⊆
      reader), and P4 (mutation stays typed).
- [ ] `proptest` added to `[dev-dependencies]` only; no runtime dependency added.
- [ ] (Optional) `fuzz/` crate present with `cbor_decode` + `validate_wire` targets, seed corpus,
      and `README.md`; **not** a workspace member; not in `verify.sh`.
- [ ] **No production code changed** (git diff touches only test modules, `Cargo.toml`
      `[dev-dependencies]`, and the optional excluded `fuzz/` crate).
- [ ] `scripts/verify.sh` is green (fmt, clippy `-D warnings` pedantic, tests) on the exact CI
      command.

---

## 11. Open questions

1. **Unit-test home (D1):** inline `#[cfg(test)] mod tests` in `cbor.rs` (recommended, matches
   convention) vs. a separate `tests/cbor_reader.rs`. Recommend inline; confirm with reviewers if
   they prefer to cap `cbor.rs` length.
2. **proptest regression persistence (D2):** commit `proptest-regressions/` seed files (default,
   pins found failures for CI) vs. `failure_persistence: None` (no committed artifacts). Recommend
   committing.
3. **`MAX_DEPTH` visibility for the boundary test:** `MAX_DEPTH` is a private `const` in `cbor.rs`
   (`cbor.rs:27`). The inline unit test can read it directly (same module); an integration test
   could not. This is another reason the depth boundary case belongs in the **inline** module. If a
   future refactor moves depth tests out, expose the cap (e.g. re-export or a `pub(crate)` const) —
   do **not** hard-code `16`.
4. **Fuzz target scope:** land only `cbor_decode`, or both `cbor_decode` and `validate_wire`?
   Recommend both if the fuzz crate is done at all; otherwise defer the whole `fuzz/` crate (it is
   explicitly optional in spec §11.4) and land unit + property tests now.
5. **Mutation-property strength (P4):** assert only "no panic + typed" (safe, always true) vs. also
   asserting `Err` for provable mutation classes. Recommend the weaker invariant as the gate and the
   stronger `Err` assertion where it is provably correct (mutations inside the `signed` region).

---

## 12. Assumptions

- The strict reader's **current behavior is the intended contract** — these tests *characterize and
  lock* it (spec §3 deterministic profile, §10.3 no-panic). If a test reveals a deviation from RFC
  8949 §4.2.1 / the spec, that is a bug for a separate issue, not a test to bend to the code.
- `proptest 1.x` resolves cleanly in the workspace `Cargo.lock` and is acceptable as a new
  dev-dependency (dev-only, no impact on the shipping dependency tree or `--no-default-features`
  consumers).
- The CI gate remains exactly `scripts/verify.sh` (stable toolchain, workspace members only), so a
  nightly `cargo-fuzz` crate must stay workspace-excluded to avoid entering the gate.
- `MAX_DEPTH = 16` and the `constants.rs` caps are stable for MVP; tests import them rather than
  duplicating literals.
- No new golden byte-vectors are introduced beyond what `PHASE-0-SPIKE.md` already pins; the unit
  cases craft minimal single-defect CBOR by hand and assert *variants/structure*, not new pinned
  hashes (avoids the R2 "un-reproduced fixture" trap from the parent spec).
```
