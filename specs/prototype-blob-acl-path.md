# Spec: Prototype Blob ACL Path (Blob Plane membership + per-hash authorization)

| | |
|---|---|
| **Issue** | #13 — [IR-0009] Prototype blob ACL path |
| **Parent** | #1 (Phase 0 epic) |
| **Labels** | type/spike, type/security, area/blob, priority/p1, risk/medium |
| **Traceability** | `PRD.v0.3.md` §9.2 (Blob Plane), §15.6 (Share File), §18.2 (Availability Risk); also §11.2 (`file.shared`), §13.1/§13.3.6 (content-addressed, verified artifacts), §14 (Availability Model). `PHASE-0-SPIKE.md` Membership & Ordering §5 (pipe/blob authorization at connect-accept time), Spike Plan **Day 8** (Blob Plane ACL confirmation), Test Vectors §16, Event Protocol §7 (`file.shared` schema), Pinned Crate Versions (`iroh-blobs` note) + Residual Risks #3, #10. |
| **Status** | Implemented — all ACs met. Findings: `crates/spike-blobs/NOTES.md`. |
| **Type** | Spike / prototype (throwaway-grade code + a written findings note). No production-code or shipping-API commitment. |

---

## 1. Summary

Build a **spike** that confirms the Blob Plane can enforce **room membership** (per-node
authorization) and **per-hash authorization** for shared artifacts, end to end, over real
`iroh-blobs`. This is a *confirmation* spike, not a risk burn-down: per `PHASE-0-SPIKE.md`
the Blob Plane is a "near-free win" on top of a working event log, so the goal is to prove
the ACL path actually works against the chosen `iroh-blobs` version and to **document the
version choice and the real ACL API surface** so the MVP can wire it without surprises.

Concretely, the deliverable (`spike-blobs`) must demonstrate, against a live provider and a
live fetcher:

1. **Import/serve** a local blob through `iroh-blobs`.
2. **Create and consume** a `file.shared` reference (Event Protocol §7) — i.e. the fetcher
   learns `blob_hash` + provider from a `file.shared` payload, then fetches.
3. **Deny non-member fetches** at connect (or request) time.
4. **Deny unreferenced-hash fetches** even for an Active member (per-hash gate).
5. **Verify fetched bytes** by recomputing BLAKE3-256 and requiring equality with the
   declared `blob_hash`.
6. **Report "unavailable"** honestly when no provider is online (PRD §14/§18.2).

This document is detailed enough to execute without re-deriving scope. It deliberately does
**not** implement a shipping Blob Plane and **must not modify production code** in
`iroh-rooms-core` / `iroh-rooms-cli` — the spike lives in its own throwaway crate.

---

## 2. Background & current repository state

**Read before starting:**

- `PHASE-0-SPIKE.md`:
  - **Membership & Ordering §5** — the normative blob/pipe authorization model:
    *proven identity* (QUIC/TLS-authenticated remote `EndpointId` = `device_id`, resolved to
    `sender_id` via the device binding), *snapshot + fail-closed*, and the **Blob serve gate**:
    `EventMask` with `connected = Intercept` and `get`/`get_many = Intercept`; accept on
    `ClientConnected` iff the connecting identity ∈ Active members; serve a hash on
    `RequestReceived` only if it is referenced by a valid `file.shared` authored by an Active
    member and causally visible, else return `AbortReason::Permission`.
  - **Spike Plan Day 8** — the exact deliverable: *"member fetches, non-member denied at
    connect, wrong/unreferenced hash denied (Test Vectors §16)"*; soft GATE: **GO iff per-hash
    and per-node gating both deny correctly.**
  - **Test Vectors §16** — the authoritative scenario (snapshot, who is accepted/rejected,
    and the byte-verification rule). `E_file.content.blob_hash =
    dd101e8f6fcf005b1dd4780c4f7b736c4f456ce292e50a896d1f40df6dbef313`.
  - **Event Protocol §7 `file.shared`** — the reference schema (`blob_hash bstr[32]`,
    `blob_format` opt `"raw"|"hash_seq"`, `providers` opt `[EndpointId]` default `[device_id]`,
    plus `file_id`, `name`, `mime_type`, `size_bytes`). Verifier rule: *recompute BLAKE3-256
    and require it equals `blob_hash`.*
  - **Pinned Crate Versions** — `iroh-blobs =0.103.0` (modern `provider::events` ACL, on
    iroh 1.0, maintainer-labeled pre-production) vs `=0.35.0` ("production" but on iroh 0.35,
    **no** events ACL). Also the standing warning that all version/API names were gathered by
    web recon and **must be confirmed against crates.io / docs.rs**.
  - **Residual Risks** #3 (blob-serve-to-any-Active-member + no per-blob revocation) and #10
    (`0.103` vs `0.35` shipping decision deferred to MVP time).
- `PRD.v0.3.md` §9.2 (blobs are referenced by events, never carried inline; the 6-step file
  flow), §15.6 (Share File acceptance criteria incl. integrity verification + honest
  unavailable reporting), §11.2 (`file.shared` event), §18.2 (Availability Risk).

**Current repo state (relevant):**

- Rust workspace, two crates: `crates/iroh-rooms-core` (placeholder: only `PROTOCOL_VERSION`)
  and `crates/iroh-rooms-cli` (binary scaffold). **No protocol code exists yet** — no
  event-core, no membership fold, no transport. `Cargo.toml` has **no** iroh dependencies.
- Workspace lints are strict: `unsafe_code = "forbid"`, clippy `all` + `pedantic` = warn.
  `scripts/verify.sh` runs fmt + clippy + tests across the workspace.
- `specs/` exists (this file joins `getting-started-demo-script.md`).

**Dependency implication (important):** the event-core (Event Protocol §1–§6) and the
membership causal fold (Day 7) are **not implemented yet**. This spike therefore must **not**
block on them. It uses a **minimal in-memory authorization fixture** (an Active-member set +
a referenced-hash set, populated to match Test Vectors §16) with the *same shapes* the real
membership fold will later produce, so it can be re-pointed at the real fold without
reshaping the gate. See §6.3.

---

## 3. Goal, scope, and non-goals

### 3.1 Goal (what "confirmed" means)

Prove, on a running provider/fetcher pair, that the `iroh-blobs` provider events hook can
implement the §5 blob serve gate: **per-node** admission (Active-member allowlist by
authenticated `EndpointId`) **and** **per-hash** authorization (serve only hashes referenced
by a valid `file.shared`), with **content-hash verification** by the receiver and **honest
unavailable reporting** when no provider serves the hash.

### 3.2 In scope

- Stand up an `iroh-blobs` store; import a local file as a blob; serve it over an iroh
  `Endpoint`/`Router`.
- Construct a `file.shared` payload (CBOR map per §7) and have the fetcher consume it to learn
  `blob_hash` + provider `EndpointId`(s).
- Implement the two-gate ACL via `provider::events` (connect gate + request gate).
- Fetch as an authorized member; recompute BLAKE3-256 over the assembled bytes; assert it
  equals `blob_hash`.
- Negative paths: non-member denied; Active member denied an unreferenced hash; wrong/tampered
  hash detected; no-provider-online → clean "unavailable" report.
- A written **findings note** capturing the version decision and the real ACL API.

### 3.3 Out of scope / non-goals

- The real event log, signing, canonical CBOR, or causal membership fold (mocked via fixtures
  here; built in IR-0007/Day 2–3 and Day 7 work).
- The real transport plane / full-mesh peer manager (the spike uses a direct connection
  between two local endpoints; multi-machine real-NAT runs are Day 1 / Day 10's job).
- `hash_seq` / collection trees (default to `"raw"` single-blob; note `hash_seq` as an
  observation, optionally smoke-test if cheap).
- Per-blob revocation (Residual Risk #3 — none in MVP; document, do not build).
- Tear-down-on-learn for **blobs** (the spike documents it; live revocation mid-transfer is a
  pipe-plane concern in Day 9 / Test Vector §17, not required here).
- Any `0.103` vs `0.35` **shipping** commitment (Residual Risk #10 — record evidence, defer
  the call to MVP time).
- CLI UX/polish. Spike binary only.
- Modifying `iroh-rooms-core` / `iroh-rooms-cli` production code.

---

## 4. Placement & dependencies

### 4.1 Where the spike lives

Create a **new, isolated, throwaway crate** so the spike never contaminates the shipping
crates' dependency tree:

```
crates/spike-blobs/
  Cargo.toml
  src/main.rs            # the runnable demo (provider + fetcher, all scenarios)
  src/acl.rs             # the authorization fixture + gate logic (the reusable shape)
  src/file_shared.rs     # minimal file.shared CBOR encode/decode for the spike
  tests/blob_acl.rs      # automated scenarios (Test Vectors §16) where feasible
  NOTES.md               # the findings note (version + ACL API observations) — a deliverable
```

Add `crates/spike-blobs` to the workspace `members` list **only for the spike**; if leaving
it in `main` is undesirable, gate it behind a `spikes` workspace or mark it clearly. (Open
question OQ-1.) Keep it out of any shipping build target.

### 4.2 Dependencies to add (spike crate only)

Pin exact versions and **confirm each on crates.io / docs.rs first** (the spike doc's version
table is web-recon, not verified in-repo):

- `iroh = "=1.0.0"` (or the confirmed 1.x) — `Endpoint`, `Router`, ALPN `ProtocolHandler`,
  discovery, relay, `EndpointId`/`SecretKey`.
- `iroh-blobs = "=0.103.0"` (**primary**) — store, provider, `provider::events` ACL.
- `iroh-base` (matching iroh) — `EndpointId` key type, tickets.
- `blake3 = "1"` — independent content-hash verification.
- `tokio` (async runtime), `anyhow`/`thiserror` (errors), `tracing` + `tracing-subscriber`
  (observe ACL decisions), and a CBOR codec for `file.shared` (`ciborium`, to match the
  Event Protocol §3 choice).

> The strict workspace lints (`pedantic`, `unsafe_code = forbid`) apply. Keep the spike clippy-clean
> so `scripts/verify.sh` stays green, or explicitly scope the spike out of the verify gate
> (OQ-1). Do **not** weaken the workspace lints to make spike code pass.

---

## 5. Design — the blob serve gate

### 5.1 Identity model (recap, from §1 / §5)

- The blob provider runs on an iroh `Endpoint`. A fetcher dials it; iroh TLS client-auth makes
  the provider's view of the remote `EndpointId` a **cryptographically proven** identity.
- `EndpointId == device_id` (Event Protocol §1). Authorization is tracked against
  **`sender_id` (identity)**, so the gate resolves `device_id → bound sender_id → Active?` via
  the validated device binding. In the spike, this resolution is a fixture map (§6.3).

### 5.2 Two enforcement points (both required for the GATE)

**Gate 1 — per-node admission (connect time).** Reject any peer whose authenticated identity
is not an **Active** member, before serving anything. Per §5 the mechanism is `EventMask`
`connected = Intercept` → on `ClientConnected`, accept iff identity ∈ Active; else reject.
*Defense-in-depth option:* also reject unknown `remote_endpoint_id` at the iroh `Router`
`accept()` for the blobs ALPN (the same early gate the Event Plane uses). The spike must at
minimum exercise the `provider::events` connect gate; the Router-level gate is an observation.

**Gate 2 — per-hash authorization (request time).** Even for an Active member, serve a hash
only if it is referenced by a valid `file.shared` (authored by an Active member, causally
visible). Mechanism: `EventMask` `get`/`get_many = Intercept` → on `RequestReceived`, if
`request.hash ∉ referenced_hashes` return `AbortReason::Permission`.

> **Spike simplification of "valid `file.shared` authored by an Active member and causally
> visible":** the real rule needs the event-core + membership fold. The spike collapses it to
> a precomputed `referenced_hashes: Set<Hash>` derived from the fixture `file.shared`
> payload(s). This preserves the *gate's* behavior (the thing under test) while stubbing the
> *provenance* (built later). Document this simplification in `NOTES.md`.

### 5.3 Receiver-side content verification

Independently of any provider gate, the **fetcher** must, after assembling bytes, compute
`BLAKE3-256(bytes)` and require it equals the `file.shared.blob_hash`. `iroh-blobs` performs
verified streaming against the requested hash during transfer (observe and document this), but
the spike still recomputes independently to (a) satisfy the explicit AC and (b) catch a
`file.shared` that *declares* a hash different from the bytes actually transferred (a
mismatch/tamper of the reference itself).

### 5.4 Unavailable-provider behavior

When no provider holds/serves the hash (provider offline, or hash never imported), the fetch
must fail **cleanly and promptly** (bounded timeout) and the spike must surface a distinct
**"unavailable"** outcome — not a hang, not a panic, not a generic error. This mirrors PRD §14
("files fetchable only when at least one peer with the file is online") and §15.6.6 / §18.2
(honest unavailable reporting).

### 5.5 Decision summary (gate → outcome)

| Connecting peer | Requested hash | Gate 1 (connect) | Gate 2 (per-hash) | Receiver verify | Outcome |
|---|---|---|---|---|---|
| Active member (Carol) | referenced (`dd101e8f…f313`) | accept | serve | `BLAKE3==hash` ✓ | **fetch succeeds** |
| Removed member (Dave) | referenced | **reject** | — | — | denied at connect |
| Non-member (Mallory) | referenced | **reject** | — | — | denied at connect |
| Active member (Carol) | **unreferenced** hash present in store | accept | **`Permission`** | — | denied per-hash |
| Active member (Carol) | referenced, but bytes/declared-hash mismatch | accept | serve | `BLAKE3!=hash` ✗ | **rejected by receiver** |
| Active member (Carol) | referenced, **no provider online** | n/a | n/a | n/a | **"unavailable"** reported |

(Active/Removed/Non-member roster per Test Vector §16: `{Alice: Active(admin), Bob: Active,
Carol: Active, Dave: Removed}`, Mallory unknown.)

---

## 6. Implementation steps

Work top to bottom; each step is independently observable.

### 6.1 Step 0 — Confirm the ground truth (do this first; it gates everything)

1. On crates.io / docs.rs, **confirm** the latest/usable `iroh-blobs` line and the iroh version
   it pins. Record the exact versions. (The doc's `0.103.0`/`iroh 1.0.0` pairing is unverified
   recon; the doc also notes a prior recon pass reported `iroh-blobs 0.97`.)
2. Read the `iroh-blobs` `provider`/`provider::events` API docs for the **confirmed** version
   and map the spike-doc names to reality. The doc cites `EventMask`, `RequestMode::Intercept`,
   `ConnectMode`, `ClientConnected`, `RequestReceived`, `AbortReason::Permission`, `get`,
   `get_many`, `remote_node_id`. **These names may differ in the actual crate** — capture the
   real types/enum variants/method signatures in `NOTES.md` as you go. If the events ACL is
   absent or differs materially, that itself is the spike's key finding (see §8 risks).
3. Decide the spike's working version: default **`iroh-blobs 0.103`** (the only line with the
   per-node/per-hash events ACL). If unavailable, fall back to the nearest line that exposes an
   equivalent provider-events hook and record the substitution.

### 6.2 Step 1 — Scaffold the spike crate

Create `crates/spike-blobs` per §4, add the confirmed deps, wire `tracing` so every ACL
decision (`accept`/`reject`/`Permission`) logs with the peer identity and hash. Verify it
builds and is clippy-clean (or scoped out of `verify.sh` per OQ-1).

### 6.3 Step 2 — Authorization fixture (`acl.rs`)

Define the minimal, fold-shaped authorization context:

```rust
struct AuthContext {
    // device_id (EndpointId) -> identity (sender_id); the validated device binding.
    device_to_identity: HashMap<EndpointId, IdentityKey>,
    // identities currently Active (admin counts as Active).
    active_members: HashSet<IdentityKey>,
    // hashes referenced by a valid file.shared authored by an Active member.
    referenced_hashes: HashSet<Hash>,
}

impl AuthContext {
    fn is_active(&self, peer: EndpointId) -> bool { /* resolve binding, check active set */ }
    fn is_referenced(&self, hash: Hash) -> bool { self.referenced_hashes.contains(&hash) }
}
```

Populate it to mirror **Test Vector §16**: Alice (admin)/Bob/Carol Active, Dave Removed,
Mallory absent; `referenced_hashes = { blob_hash from the file.shared payload }`. Keep the
resolution logic identical in shape to what the real membership fold will feed, so the fixture
is a drop-in seam, not a rewrite. Document the seam in `NOTES.md`.

### 6.4 Step 3 — Minimal `file.shared` (`file_shared.rs`)

Encode/decode just enough of the §7 `file.shared` `content` map (deterministic CBOR via
`ciborium`): `file_id`, `name`, `mime_type`, `size_bytes`, `blob_hash` (raw 32-byte bstr),
`blob_format` (default `"raw"`), `providers` (default `[provider device_id]`). This is the
"create/consume a `file.shared` reference" requirement: the provider **creates** it after
import; the fetcher **consumes** it to obtain `blob_hash` + provider address. No signing here
(that's event-core); note the omission.

### 6.5 Step 4 — Provider: import + serve with the gate

1. Build the provider `Endpoint` + `iroh-blobs` store; **import** a local test file → obtain
   its `Hash` (this is the content-addressing step, PRD §9.2 step 2). Assert the import hash
   equals the BLAKE3-256 of the file bytes.
2. Build the `file.shared` payload from the import result.
3. Register the blobs provider on the `Router` with the events `EventMask` configured for
   `connected/get/get_many = Intercept`.
4. Implement the handler:
   - `ClientConnected` → `AuthContext::is_active(remote)` ? proceed : reject (Gate 1).
   - `RequestReceived` → `AuthContext::is_referenced(request.hash)` ? serve :
     `AbortReason::Permission` (Gate 2).
   - Log each decision.
5. **Also import a second blob that is NOT referenced** by any `file.shared` (so the store
   physically holds an unreferenced hash) — this is what makes the Gate 2 test meaningful for
   an Active member.

### 6.6 Step 5 — Fetcher: consume reference, fetch, verify

1. Build the fetcher `Endpoint` (a distinct identity).
2. Consume the `file.shared` payload → read `blob_hash` + provider address.
3. Dial the provider and request `blob_hash`.
4. On success: assemble bytes, compute `BLAKE3-256`, assert `== blob_hash`; write the bytes
   out and confirm they match the original file.
5. Wrap the fetch in a **bounded timeout**; classify the result as
   `Fetched | DeniedAtConnect | DeniedPerHash | HashMismatch | Unavailable`.

### 6.7 Step 6 — Drive every scenario (the §5.5 matrix)

Run, as separate identities/invocations, each row of the §5.5 / Test Vector §16 matrix:

1. **Authorized fetch** — Carol fetches `dd101e8f…f313` → `Fetched`, verify passes.
2. **Non-member at connect** — Mallory (not in roster) → `DeniedAtConnect`.
3. **Removed member at connect** — Dave (Removed) → `DeniedAtConnect`.
4. **Unreferenced hash** — Carol requests the second (unreferenced) store hash →
   `DeniedPerHash` (`Permission`), proving per-hash gating is independent of node admission.
5. **Wrong/tampered hash** — point `file.shared.blob_hash` at a hash whose bytes differ (or
   flip a content byte before verify) → receiver returns `HashMismatch`.
6. **Unavailable provider** — fetch with the provider stopped / hash never imported →
   `Unavailable` within the timeout, reported cleanly.

Prefer encoding 1–5 as automated `tests/blob_acl.rs` cases (two in-process endpoints); #6 too
if a clean shutdown is reproducible. `main.rs` runs the full narrated demo with `tracing`
output for the human-readable confirmation.

### 6.8 Step 7 — Write `NOTES.md` (a required deliverable)

This is the **"Version choice and ACL API observations are documented"** acceptance criterion.
Capture (see §7 for the full checklist) the confirmed versions, the real `provider::events`
API, where it diverged from the spike doc, the two-gate wiring that worked, the verified-stream
behavior, the unavailable-state behavior, and any limitations/footguns for the MVP.

---

## 7. `NOTES.md` content checklist (the documented findings)

The findings note must record at least:

1. **Version decision & evidence** — confirmed `iroh-blobs` version actually used, the iroh
   version it pins, and the crates.io/docs.rs confirmation. State the **`0.103` vs `0.35`**
   trade explicitly (events ACL on iroh 1.0, pre-production vs no events ACL on iroh 0.35) and
   note this is the MVP-time call per Residual Risk #10 — the spike only validates the `0.103`
   ACL path; it does **not** commit the shipping line.
2. **ACL API as found** — the real types/enums/signatures for the connect gate and the request
   gate, with every divergence from the spike doc's names (`EventMask`, `RequestMode::Intercept`,
   `ConnectMode`, `ClientConnected`, `RequestReceived`, `AbortReason::Permission`,
   `remote_node_id`, `get`/`get_many`). Note whether the authenticated remote identity is
   exposed at the connect hook and again at the request hook.
3. **Two-gate wiring** — exactly how connect-time node admission and request-time per-hash
   authorization were configured, and confirmation that **both deny correctly** (the soft GATE).
4. **Verified streaming** — whether `iroh-blobs` rejects corrupted/wrong bytes itself during
   transfer (bao verified streaming) and how that interacts with the independent BLAKE3 recheck.
5. **Unavailable behavior** — the error/timeout surfaced when no provider serves the hash, and
   how to map it to honest CLI language (PRD §14/§18.2).
6. **`raw` vs `hash_seq`** — what `blob_format` modes mean for the gate (a `hash_seq` references
   child hashes; note whether the per-hash gate must allow the children).
7. **Limitations carried into MVP** — Residual Risk #3 (any Active member can serve any
   referenced blob to any other Active member; no per-blob revocation), tear-down-on-learn not
   covered for blobs, and the fixture/provenance seam that the real membership fold must fill.

---

## 8. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | `iroh-blobs` version/API names in the spike doc are recon-only and may not match the real crate (provider-events API may be renamed, restructured, or absent). | High | High | Step 0 confirms versions + API on docs.rs before coding; treat the mapping as the spike's primary finding; if the events ACL is missing/weaker, document and evaluate the Router-`accept()`-only fallback. |
| R2 | `provider::events` connect hook may not expose the **authenticated** remote identity (only an address), undermining Gate 1. | Medium | High | Verify the hook surfaces the proven `EndpointId`; if not, gate at the iroh `Router` `accept()` (where `remote_endpoint_id` is proven) and document that the per-hash gate then carries the rest. |
| R3 | Spike under-tests reality by mocking membership/provenance via fixtures. | Medium | Medium | Keep the fixture fold-shaped (§6.3); scope is explicitly the *gate*, not the *fold*; record the seam in `NOTES.md`. |
| R4 | `0.103` is maintainer-labeled pre-production; instability or bugs in the events path. | Medium | Medium | Spike-only; flag any instability for the Residual Risk #10 shipping decision. |
| R5 | Strict workspace lints (`pedantic`, `unsafe_code=forbid`) break `verify.sh` on spike code. | Medium | Low | Keep spike clippy-clean, or scope the spike crate out of the verify gate (OQ-1) without relaxing workspace lints. |
| R6 | "Unavailable" path hangs instead of erroring (no bounded timeout). | Medium | Medium | Always wrap fetch in a bounded timeout; assert a distinct `Unavailable` outcome. |
| R7 | `hash_seq`/collection blobs need child-hash authorization the per-hash gate doesn't model. | Low | Medium | Default to `raw`; document `hash_seq` implications; optionally smoke-test. |

---

## 9. Acceptance criteria

Maps issue #13 ACs + Day-8 soft GATE + Test Vector §16 to this spike. All must hold.

- [x] **AC1 — Active member can fetch a referenced blob.** Carol (Active) fetches
  `dd101e8f…f313`; transfer completes (§6.7 #1).
- [x] **AC2 — Non-member denied at connect or request time.** Mallory (non-member) and Dave
  (Removed) are denied — at the connect gate where possible, else at request time (§6.7 #2–#3).
- [x] **AC3 — Active member cannot fetch an unreferenced hash through the room ACL path.** Carol
  requesting a store hash not referenced by any `file.shared` is denied with `Permission` /
  equivalent (§6.7 #4) — proving per-hash gating independent of node admission.
- [x] **AC4 — Receiver verifies content hash after fetch.** The fetcher recomputes BLAKE3-256
  over the assembled bytes and requires equality with `file.shared.blob_hash`; a tampered/wrong
  hash is rejected (§6.7 #5).
- [x] **AC5 — Version choice and ACL API observations are documented.** `NOTES.md` covers the
  §7 checklist (confirmed version, real ACL API + divergences, two-gate wiring, verified
  streaming, unavailable behavior, MVP limitations).
- [x] **AC6 — `file.shared` is created and consumed.** Provider creates a §7-shaped
  `file.shared`; fetcher consumes it to obtain `blob_hash` + provider (§6.4/§6.6).
- [x] **AC7 — Unavailable provider reported honestly.** With no provider online, fetch fails
  cleanly within a bounded timeout and surfaces a distinct `Unavailable` outcome (§6.7 #6;
  PRD §14/§18.2) — covers the test plan's "unavailable provider state".
- [x] **AC8 — Day-8 soft GATE.** Both per-hash and per-node gating deny correctly.
- [x] **AC9 — No production code changed.** `iroh-rooms-core` / `iroh-rooms-cli` untouched;
  spike isolated in `crates/spike-blobs`; `scripts/verify.sh` remains green (or the spike is
  explicitly scoped out per OQ-1).

**Test plan coverage (issue):** authorized member → AC1; unauthorized peer → AC2; wrong hash →
AC3 + AC4; unavailable provider state → AC7.

---

## 10. Assumptions

1. `iroh-blobs` (the confirmed version near `0.103`) exposes a `provider::events`-style hook
   able to intercept connect and `get`/`get_many` and abort with a permission error. If it does
   not, the spike's job becomes documenting that gap (R1) and the Router-`accept()` fallback.
2. The provider-events connect hook exposes the **proven** remote `EndpointId` (else R2 fallback).
3. Mocking membership/referenced-hashes via fixtures is acceptable for a *gate* confirmation
   spike, given event-core and the membership fold are not yet built.
4. Two in-process iroh endpoints (or two local processes) suffice; real-NAT/multi-machine runs
   are Day 1 / Day 10's responsibility, not this spike's.
5. `raw` single-blob is the representative case; `hash_seq` is an observation, not a requirement.
6. The Test Vector §16 hash (`dd101e8f…f313`) is used as the canonical referenced hash; if the
   spike imports a different local file, its real import hash is used and the §16 value is noted
   as the protocol fixture (the *behavior*, not the literal bytes, is what's normative).

---

## 11. Open questions

- **OQ-1 (workspace integration):** Should `crates/spike-blobs` be a permanent workspace member
  (and thus subject to `verify.sh`), an opt-in spikes workspace, or live on a throwaway branch
  only? Recommended: isolated crate kept clippy-clean and included, so CI proves it builds, with
  a follow-up to remove or graduate it.
- **OQ-2 (ACL layer):** If the provider-events connect hook does not surface a proven identity,
  is gating at the iroh `Router` `accept()` (per-node) plus the request hook (per-hash)
  acceptable for the spike's confirmation, or must both live inside `provider::events`?
- **OQ-3 (`hash_seq`):** Does the MVP need `hash_seq`/collection sharing in scope soon enough to
  warrant the spike validating child-hash authorization now, or is `raw` sufficient for Phase 0?
- **OQ-4 (shipping line):** Does any spike finding (instability, missing API) change the default
  lean toward `0.103` for shipping, or does it stay the documented MVP-time decision (Residual
  Risk #10)?
- **OQ-5 (verified streaming vs recompute):** If `iroh-blobs` already guarantees verified
  streaming against the requested hash, is the independent BLAKE3 recompute kept as belt-and-
  suspenders (recommended, and required by AC4) or dropped in the eventual MVP? Document the
  recommendation.

---

## 12. Definition of done

1. `crates/spike-blobs` builds and runs the §5.5 scenario matrix, demonstrating AC1–AC8.
2. Automated tests cover the gate scenarios (§6.7 #1–#5 at minimum; #6 if reproducible).
3. `NOTES.md` satisfies the §7 checklist (AC5).
4. No production code changed; `scripts/verify.sh` green or spike explicitly scoped out (AC9).
5. Findings feed the Day-10 Phase-0 memo and the Residual Risk #10 shipping decision.
