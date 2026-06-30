# IR-0009 Blob ACL Spike — Findings (`NOTES.md`)

This is the written findings deliverable required by **AC5** (and the §7 checklist
in `specs/prototype-blob-acl-path.md`). It records the version decision, the real
`iroh-blobs` `provider::events` ACL API as found, the two-gate wiring, verified
streaming, unavailable behavior, `raw` vs `hash_seq`, and the limitations carried
into the MVP.

> Status: throwaway-grade spike. It validates the `0.103` ACL path; it does **not**
> commit a shipping line, modify the production crates, or build the membership
> fold.

---

## 1. Version decision & evidence

| Crate | Pinned | iroh line it requires | Evidence |
|---|---|---|---|
| `iroh-blobs` | `=0.103.0` | `iroh ^1.0` (resolved `1.0.1`) | crates.io sparse index `https://index.crates.io/ir/oh/iroh-blobs`, fetched 2026-06-29 |
| `iroh` | `=1.0.1` | — | sparse index `…/ir/oh/iroh`, latest `1.0.1` |
| `iroh-base` | (transitive `1.0.1`) | — | `EndpointId`/`SecretKey` key types |

**Confirmed against the index (Step 0):** `iroh-blobs 0.103.0` is the newest line and
its `[dependencies]` pin `iroh = "^1.0.0"`, `iroh-base = "^1.0.0"`,
`iroh-tickets = "^1.0.0"`, `irpc = "^0.17.0"`. The spike doc's recon (`0.103.0` on
iroh `1.0`) was therefore **correct**; the earlier "iroh-blobs 0.97" recon note was
stale. `0.103.0` is the only published line carrying the `provider::events` ACL.

**`0.103` vs `0.35` trade (Residual Risk #10 — MVP-time call, NOT made here):**

- `iroh-blobs 0.103` → modern `provider::events` ACL (per-node connect intercept +
  per-request intercept), but on iroh `1.0` and maintainer-labeled pre-production.
- `iroh-blobs 0.35` → "production-ish" but on iroh `0.35` with **no** `provider::events`
  ACL; the room ACL would have to live entirely at the `Router::accept()` layer
  (per-node only) with no per-hash gate inside the blobs protocol.

The spike validates the `0.103` ACL path because it is the only line that can satisfy
**both** gates. The shipping-line decision stays deferred to MVP time.

---

## 2. ACL API as found (divergences from the spike doc in **bold**)

Module: `iroh_blobs::provider::events`. The doc's names mostly survived; the wiring
differs.

| Spike-doc name | Real API (`0.103.0`) | Notes |
|---|---|---|
| `EventMask` | `EventMask { connected, get, get_many, push, observe, throttle }` | struct of per-event modes; `EventMask::DEFAULT` disables all (and `push = Disabled`) |
| `connected = Intercept` | `ConnectMode::Intercept` | as documented |
| `get`/`get_many = Intercept` | `RequestMode::Intercept` | **also** `NotifyLog`/`InterceptLog`/`Disabled` variants exist |
| `ClientConnected` | `ProviderMessage::ClientConnected(WithChannels<ClientConnected, _>)` | identity field is **`endpoint_id: Option<EndpointId>`** (see R2 below) |
| `RequestReceived` | `ProviderMessage::GetRequestReceived(WithChannels<RequestReceived<GetRequest>, _>)` | hash at **`msg.request.hash`**; separate `GetManyRequestReceived` for multi |
| `AbortReason::Permission` | `AbortReason::Permission` (also `RateLimited`) | as documented |
| `remote_node_id` | **`connection.remote_id()`** internally → surfaced as `ClientConnected.endpoint_id` | the doc's `remote_node_id` is now `endpoint_id` |

**How the handler is actually wired (this is the key divergence):** there is no
trait you implement. Instead:

1. `let (events, mut rx) = EventSender::channel(capacity, mask);` gives an
   `EventSender` plus a **`tokio::sync::mpsc::Receiver<ProviderMessage>`**.
2. Hand the `EventSender` to `BlobsProtocol::new(&store, Some(events))`.
3. Drive `rx.recv()` in your own task and, for each intercepted message, answer on
   `msg.tx` — `msg.tx.send(Ok(()))` to allow or `msg.tx.send(Err(AbortReason::Permission))`
   to deny. `WithChannels` derefs to the inner event, so `msg.endpoint_id` /
   `msg.request.hash` read straight through.

See `src/net.rs::spawn_event_gate` for the concrete loop.

**R2 resolved — the connect hook DOES expose the proven identity.** `ClientConnected`
carries `endpoint_id: Option<EndpointId>`, populated by the provider from
`connection.remote_id()` (the QUIC/TLS-authenticated remote id). So Gate 1 can run
inside `provider::events` and the Router-`accept()` fallback (OQ-2) is **not** needed
for the spike. We still treat `endpoint_id == None` as fail-closed (deny).

**Quirk worth flagging for the MVP:** `EventSender::request()` keys the request-mode
decision off `self.mask.get` for *all* request kinds (get and get_many), even though
the dispatched message variant is correctly typed. Set `get` and `get_many` to the
same mode to avoid surprises (we set both to `Intercept`).

---

## 3. Two-gate wiring (and confirmation both deny — the Day-8 soft GATE / AC8)

- **Gate 1 (per-node admission):** `mask.connected = ConnectMode::Intercept` →
  on `ProviderMessage::ClientConnected`, resolve `endpoint_id → identity → Active?`
  via `AuthContext`; allow or `AbortReason::Permission`. The provider then
  `connection.close(ERR_PERMISSION, …)`.
- **Gate 2 (per-hash):** `mask.get = mask.get_many = RequestMode::Intercept` →
  on `GetRequestReceived`, allow iff `auth.is_referenced(msg.request.hash)`; else
  `AbortReason::Permission`, which resets the response stream with `ERR_PERMISSION`.

`push` stays `Disabled` and `observe` stays `None`, so peers cannot write to or
enumerate the store over the blobs ALPN.

Both denials are **observed independently** (see `tests/blob_acl.rs`):

- non-member / removed-member → denied at connect (`DeniedAtConnect`);
- Active member requesting an unreferenced (but physically present) hash → denied
  per-hash (`DeniedPerHash`).

Because Gate 2 fires for an *Active* member on a *present* hash, it proves per-hash
authorization is independent of node admission — the soft GATE holds: **GO**.

### Mapping denials back on the getter side

Both gates abort with `ERR_PERMISSION` (`iroh_blobs::protocol::ERR_PERMISSION`,
`VarInt(1)`), but they surface differently and are distinguishable without guessing:

- connect-gate close → the getter cannot **open** a stream → `GetError::open().is_some()`;
- per-hash reset → the response stream is reset → `GetError::remote_read()/remote_write().is_some()`.

`GetError::iroh_error_code()` returns the `VarInt`, so we confirm it is `ERR_PERMISSION`
before classifying. See `src/net.rs::classify_get_error`.

---

## 4. Verified streaming vs the independent recompute (AC4 / OQ-5)

`iroh-blobs` uses **BLAKE3 bao verified streaming**: the getter requests
`GetRequest::blob(hash)` and the transfer is validated against that hash chunk by
chunk, so a provider that serves bytes not matching the requested hash is rejected
*during* transfer (a well-behaved provider never sends invalid data; the protocol is
"paranoid about data integrity").

The spike **still** recomputes `BLAKE3-256` over the assembled bytes and compares to
`file.shared.blob_hash`, because the two checks guard different things:

- verified streaming guards *bytes vs the hash you asked for*;
- the independent recompute guards *the `file.shared` reference itself* — it catches a
  reference that **declares** a hash different from the content it points at (modelled
  in the `HashMismatch` scenario by tampering the declared hash).

**OQ-5 recommendation:** keep the independent recompute in the MVP. It is required by
AC4, it is effectively free (BLAKE3 is fast), and it is the only check that validates
the *reference*, not just the transfer.

---

## 5. Unavailable behavior (AC7) → honest CLI language

With the provider shut down (or the hash never imported), the getter's
`endpoint.connect(...)` / `get_blob(...)` does not resolve. The spike wraps the whole
attempt in `tokio::time::timeout`; on elapse it returns a distinct
`FetchOutcome::Unavailable` (no hang, no panic). A hard `connect()` error is likewise
mapped to `Unavailable`.

Map to honest UX (PRD §14 / §15.6.6 / §18.2): *"This file isn't available right now —
no online peer is sharing it. Try again when someone who has it is online."* Never
present unavailability as corruption or as a permission denial.

---

## 6. `raw` vs `hash_seq` (OQ-3 / R7)

The spike uses `blob_format = "raw"` (single content-addressed blob). The per-hash
gate matches on exactly the requested root hash.

For `hash_seq` (a collection root whose content is a sequence of child hashes), the
getter fetches the root **and** its children. `iroh-blobs` exposes these as
`GetManyRequest { hashes: Vec<Hash> }` / child gets. **Implication for the gate:** the
per-hash allowlist must include the child hashes too, or a collection fetch is denied
mid-stream. Our `GetManyRequestReceived` arm already denies unless *every* requested
hash is referenced — but in the MVP the membership fold must expand a `file.shared`
that references a `hash_seq` into `{root} ∪ {children}` when building
`referenced_hashes`. Recommend keeping Phase 0 to `raw` and treating `hash_seq` ACL as
a follow-up.

---

## 7. Limitations carried into the MVP

1. **Provenance is stubbed (R3 seam).** `referenced_hashes` and the
   `device → identity → Active` resolution come from an in-memory `AuthContext`
   fixture, not the real signed event log + causal membership fold (IR-0007 / Day 7).
   The fixture is deliberately *fold-shaped* (`src/acl.rs`): the real fold re-points
   these three collections without reshaping the gate. The `file.shared` here is
   **unsigned** — signing/canonical CBOR is event-core's job.
2. **Residual Risk #3 — blob serve granularity.** Any Active member can serve any
   referenced blob to any other Active member, and there is **no per-blob revocation**
   in the MVP. Not built here; documented.
3. **Tear-down-on-learn is not covered for blobs.** Live revocation mid-transfer is a
   pipe-plane concern (Day 9 / Test Vector §17), out of scope for this spike.
4. **Pre-production crate (R4).** `0.103` is maintainer-labeled pre-production. No
   instability surfaced in the gated connect/get path during the spike, but this feeds
   the Residual Risk #10 shipping decision.
5. **CI weight (R5 / OQ-1).** The crate is a workspace member so CI proves it builds,
   which pulls the full iroh 1.0 dependency tree into `scripts/verify.sh`. The spike
   is kept clippy-clean (workspace `pedantic` lints unchanged); if the build cost is
   undesirable, move `crates/spike-blobs` to an opt-in spikes workspace or a throwaway
   branch — without relaxing the workspace lints. Follow-up: remove or graduate this
   crate once the real Blob Plane lands.
6. **Loopback-only transport.** The spike connects two in-process endpoints over
   `127.0.0.1` with relay disabled. Real-NAT / multi-machine runs are Day 1 / Day 10's
   responsibility.

---

## 8. Structure note

The spike adds a small `src/lib.rs` (not in the spec's file list) so `src/main.rs`
(the narrated demo) and `tests/blob_acl.rs` (the automated matrix) can share
`acl` / `file_shared` / `net` / `roster`. Integration tests can only link a crate's
library target, so the shared logic must live in the lib.

## 9. Same-identity connection restriction (iroh 1.0.1 observation)

Attempting to connect a second `Endpoint` built from the same `SecretKey` as the
provider back to that provider yields `Unavailable` in iroh 1.0.1 — the transport
layer rejects same-`EndpointId` connections. This is not an ACL issue: `AuthContext`
is identity-agnostic and gives the provider's own `EndpointId` no special treatment.
Observed while writing the e2e test suite; a test that exercises this path is omitted
because the outcome is determined by iroh's transport layer, not by the gate.

**Implication for the MVP:** a room member who happens to be the blob provider and
wants to verify their own share would need a distinct endpoint identity (e.g., a
second device or a synthetic second identity) to connect as a fetcher. This is an
iroh 1.0.x behavior; record for the Residual Risk #10 shipping decision.
