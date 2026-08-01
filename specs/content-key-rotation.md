# Content-Key Rotation on Member Removal (#191)

- **Issue:** #191 — removed device retains read access to room fan-out (malicious-reader
  forward secrecy).
- **Parent epic:** security roadmap (`PRD.v0.3.md` §13.5.2 "Member removal with key
  rotation"); threat-model T27 follow-up.
- **Labels:** `type/feature`, `area/security`, `area/protocol`, `priority/p1`, `risk/high`.
- **Depends on:** #194 (terminal member-removal delivery), #195 (terminal-confirmation
  lockout bound), #196 / #197 (honest-transport gossip-plane revocation, threat-model T27).
- **Owning crates:** `crates/iroh-rooms-core` (encrypted-content wire schema, rotation fold
  semantics, key store), `crates/iroh-rooms-v2-core` **or a sibling pure crate** (X25519 /
  HKDF / AEAD key-wrap primitives), `crates/iroh-rooms-net` (key-distribution fan-out,
  decrypt-on-read), `crates/iroh-rooms-cli` (join-time key wrap, backlog-read UX change).
- **Status:** **Planning / spec only.** Do not implement from this document without a
  follow-up build task and a security sign-off. This document changes no production code.

---

## 1. Summary

Today every room event is **signed but not encrypted**. `SignedEvent.content` is part of the
canonical signed bytes (CSB) and is plaintext on the wire and in `rooms.db`. Confidentiality
is purely **admission-based**: QUIC/TLS remote-`EndpointId` gating at connect, plus the
gossip-overlay revocation landed in #196/#197.

That revocation closes the leak for an **honest** removed device whose transport runs the
shipped code (threat-model T27, Partial). It does **not** stop a **malicious** removed device
that patches its own transport: such a device can keep a process subscribed to the public
gossip topic (derived from the public `room_id`, spec `gossip-overlay-events-fan-out.md` D5 —
a rendezvous point, not a secret) and read every content event it can reach, indefinitely.

Closing that requires **forward secrecy**: rotating a room **content key** at each removal so
post-removal content is unreadable to the removed device even if it receives the ciphertext.

The non-negotiable constraints, repeated throughout this document:

- **The membership fold must keep working on plaintext.** A not-yet-`Active` invitee reads
  the never-windowed membership closure to join (`specs/room-join-by-ticket.md` §step 3), and
  every peer's fold reads membership events authored by *any* device — including removed ones
  — for fork detection and sticky-departure logic. Membership events **cannot** be encrypted.
- **The signature / causal-DAG model is preserved.** The eight signed fields, `event_id`
  (BLAKE3 of CSB), and the Ed25519 signature over CSB must keep verifying byte-for-byte, so
  dedup, causal ordering, and convergence are unchanged.

This spec therefore plans **content-event encryption under a rotating room key** — *not* a
full group E2EE ratchet (which stays out of scope, §12).

---

## 2. Repository context read (verified)

| Concern | Current state | Location | Reused / changed by this spec |
|---|---|---|---|
| Signed event model | Eight signed fields; `content: Content` is signed-over plaintext; `event_id = BLAKE3(CSB)`; Ed25519 sig over CSB | `crates/iroh-rooms-core/src/event/signed.rs:20-50` | **Preserved.** Only the *body* of content events becomes ciphertext; the envelope and signature stay cleartext. |
| Event classes | `affects_membership` = `RoomCreated`/`MemberInvited`/`MemberJoined`/`MemberLeft`/`MemberRemoved`; the rest are content | `crates/iroh-rooms-core/src/membership/fold.rs:611-620` | **Load-bearing split.** Membership stays plaintext (D1); content events (`MessageText`/`FileShared`/`PipeOpened`/`PipeClosed`/`AgentStatus`) become encryptable (D2). |
| Membership fold | Reads `content` of membership events for status/role/fork logic | `crates/iroh-rooms-core/src/membership/fold.rs` | **Unchanged** — membership bodies stay plaintext. |
| Join bootstrap | Invitee pulls the never-windowed membership sub-DAG *before* `Active`; proves invite via `capability_hash` | `specs/room-join-by-ticket.md`; `crates/iroh-rooms-core/src/sync/engine.rs:1058-1060` | **Constrains D1/D6.** Membership stays plaintext; the invitee receives content keys only *after* its join folds (D6). |
| Gossip overlay | Public `events_topic(room_id)`; honest-transport self-standing revocation (#196/#197) | `crates/iroh-rooms-net/src/gossip.rs`, `crates/iroh-rooms-net/src/node.rs` | **Unchanged.** #191 is the layer that makes a lingering malicious subscription *useless* (ciphertext, no key). |
| Single admin | `MembershipSnapshot::admin()` = the genesis signer; immutable in v1 | `crates/iroh-rooms-core/src/membership/model.rs:141-146` | **Rotation is admin-driven (D4).** Admin-offline rotation stall is a documented constraint (§6, §10). |
| Crypto primitives in dep tree | `blake3`, `ed25519-dalek` + `curve25519-dalek` (X25519 reachable), `aes-gcm`, `chacha20`, `ring`, `sha2`, `rand`; **no `hkdf`, no `x25519-dalek`/`crypto_box` yet** | `Cargo.lock` | **Add `hkdf` + `x25519-dalek` (or `crypto_box`) in the pure crate (D3).** |
| v2 pure core | `iroh-rooms-v2-core` explicitly excludes group encryption / key ratchets / payload encryption | `crates/iroh-rooms-v2-core/src/lib.rs:13` | **Decision needed (§10 OQ-1):** put the wrap/unwrap primitives in a *new* sibling pure crate vs. relaxing the v2-core invariant. |
| Plaintext storage | `rooms.db`, blobs, `audit.ndjson` plaintext; ADR-0001 scopes beta to trusted local machines | `docs/security/threat-model.md` T10, `docs/decisions/ADR-0001-local-storage-posture.md` | **Side benefit + new secret.** At-rest content becomes ciphertext, but the epoch **key store** is itself a new secret to protect (§6, T28). |

---

## 3. Goals and non-goals

**Goals**

- **G1.** After a `member.removed(D)` is folded, content events authored in subsequent epochs
  are unreadable to `D` even if `D` receives the ciphertext (forward secrecy, post-removal).
- **G2.** Membership events remain plaintext and the fold, join bootstrap, signature
  verification, dedup, and causal ordering are byte-for-byte unchanged.
- **G3.** Rotation is atomic with the fold: every honest member rotates to the new epoch at
  the same causal point, and `D` is excluded from the new key.
- **G4.** A newly-joined invitee can read room backlog after receiving the epoch keys on join.
- **G5.** The threat model moves T27 from "Partial — honest transport only" to "Controlled"
  for the malicious-reader case, with the residual risks recorded as new rows.

**Non-goals** (see §12)

- Full group E2EE ratchet / per-message forward secrecy (MLS-style).
- Metadata privacy (an observer still learns that a content event of a given type occurred).
- Multi-admin rotation, admin-offline rotation, or protection against a compromised admin.
- Encrypting membership events, blobs at rest, or `audit.ndjson`.

---

## 4. Key design decisions

### D1 — Membership events stay plaintext; only content events are encrypted

The membership/content split (`fold.rs:611`) is the encryption boundary. Membership bodies
(`RoomCreated`, `MemberInvited`, `MemberJoined`, `MemberLeft`, `MemberRemoved`) are never
encrypted: the fold and join bootstrap depend on reading them from any device. Content bodies
(`MessageText`, `FileShared`, `PipeOpened`, `PipeClosed`, `AgentStatus`) are the confidential
payload and become encryptable.

*Rejected alternative — encrypt everything:* breaks join bootstrap (an invitee cannot read the
membership closure it needs to join) and the fold's cross-device membership reads. **Out.**

### D2 — Encryption is inside `content`; the signed envelope stays cleartext

A content event's body is replaced by an **encrypted-content envelope**:

```
EncryptedContent {
    key_epoch: u64,          // which room-content-key epoch encrypted this body
    nonce: [u8; 12],         // AEAD nonce, unique per (key_epoch, event)
    ciphertext: Vec<u8>,     // AEAD(room_key[key_epoch], plaintext_cbor, aad)
}
```

- `plaintext_cbor` is the deterministic-CBOR encoding of the original content body.
- **Associated data (AAD):** the event's cleartext signed prefix — `schema_version ‖ room_id ‖
  sender_id ‖ device_id ‖ event_type ‖ created_at ‖ prev_events` — binds the ciphertext to
  this exact event so it cannot be transplanted onto another event or room.
- The eight signed fields, `event_id`, and the signature remain cleartext and computed over
  the **envelope** form (the event as transmitted). Signature verification, `event_id`
  derivation, dedup, and the fold all operate on the envelope unchanged; only a holder of
  `room_key[key_epoch]` can recover the body.

*Consequence (metadata):* `event_type` stays cleartext for routing/fold classification, so an
observer learns *that* a chat/file/pipe/agent event occurred, not its content. This matches
the accepted "no full metadata privacy" posture (§13.5). Recorded in §6.

*Schema detail to pin at build time:* whether `EncryptedContent` is a new `Content` variant
(`Content::Encrypted { inner_type, .. }`) or a per-type ciphertext field. A single
`Content::Encrypted` variant carrying `inner_type` is simpler and keeps the closed content
registry closed; the exact shape is OQ-2 (§10).

### D3 — Key wrap: X25519 ECDH → HKDF → AEAD, in a pure crate

Each member's Ed25519 device key converts to a Montgomery (X25519) key
(`curve25519-dalek` is already in-tree). The admin wraps `room_key[epoch]` to each remaining
Active member's device:

1. `shared = X25519(admin_static, member_device_pub)` — or ephemeral-X25519 for
   recipient-only forward secrecy of the *channel* (OQ-3).
2. `wrap_key = HKDF-BLAKE3/sha2(shared, info = "iroh-rooms/content-key-wrap/v1" ‖ room_id ‖ epoch)`.
3. `wrapped = AEAD(wrap_key, room_key[epoch], aad = room_id ‖ epoch ‖ member_device)`.

Primitives live in a **pure, deterministic, sans-IO crate** (same invariants as
`iroh-rooms-v2-core`): no transports, no async, no storage. Add `hkdf` and `x25519-dalek`
(or `crypto_box`) as dependencies of *that* crate only. Whether this is a new sibling crate
or a relaxation of `iroh-rooms-v2-core`'s exclusion is **OQ-1** (§10) — a new sibling keeps
the v2-core purity tripwire intact and is the default recommendation.

### D4 — Rotation is admin-driven, atomic with the fold

v1 has a **single immutable admin** (`MembershipSnapshot::admin()`). Rotation:

1. The admin folds `member.removed(D)` and, in the **same causal batch immediately after**,
  emits a new **`MemberKeyDistribution`** record (see D5) carrying `{ new_epoch,
  wrapped_keys: { device → wrapped_key } }` for every *remaining* Active member device.
2. `D` is **excluded** from the wrapped set, so `D` never receives `room_key[new_epoch]`.
3. Honest members rotate to `new_epoch` at the fold point of the distribution record — the
  same causal point on every converged node, so content authored after it uses the new key.

The admin's own pre-existing `room_key[old_epoch]` is retained for reading historical content
(D7); only *new* content uses the new epoch.

*Admin-offline stall:* if the admin is offline when a removal is needed, rotation (and thus
forward secrecy) stalls until the admin returns — the removal still revokes *access* via
#196/#197, but new content keeps using the old epoch. This is an accepted v1 constraint given
the single-admin model; recorded in §6 and §10 OQ-4.

### D5 — `MemberKeyDistribution` is a membership-adjacent cleartext-envelope record

The distribution record must be fold-visible (so every node learns `new_epoch` and rotates at
the same causal point) but its per-recipient wrapped-key blobs are openable only by that
recipient. It is a new record type with:

- **Cleartext envelope:** `room_id`, `new_epoch`, the admin's signature, `prev_events` (so it
  enters the causal DAG and the fold).
- **Per-recipient ciphertext:** a map `device_id → wrapped_key` (D3). A recipient opens only
  its own entry.

Because membership stays plaintext and this record's *envelope* is cleartext, the fold can
process it on every node; because the wrapped keys are per-recipient ciphertext, `D` learns
nothing useful even though it receives the record.

*Schema to pin:* whether `MemberKeyDistribution` is a `Content` variant processed by the fold
(preferred — it is membership-adjacent and must rotate the fold's epoch) or a separate
side-channel record. Fold-integrated is recommended so rotation is causal and convergent (OQ-2).

### D6 — Join-time key wrap gates backlog reads

A newly-joined invitee needs current + historical epoch keys to read backlog. On its
`member.joined` folding, the admin wraps the full `epoch → room_key` map to the new device
(same D3 mechanism, one `MemberKeyDistribution` targeting the invitee). Until then the
invitee can read membership (plaintext) but **not** historical content — a deliberate UX
change from "read backlog immediately," recorded in §6 and §10 OQ-5.

### D7 — Old epochs stay decryptable; forward secrecy is post-removal only

A removed device already had access to content up to its removal; rotating keys does not
(clock cannot) make that history unreadable, and that is **not** a goal. The per-node key
store is a small `epoch → room_key` map retained for reading backlog. Forward secrecy is
*post-removal*: only content authored in epochs after `D`'s removal is unreadable to `D`.

---

## 5. Validation, authorization, and error model

- **Signature/CSB invariants preserved.** `event_id = BLAKE3(CSB)` and the Ed25519 signature
  are computed over the transmitted (envelope) event. Golden-vector tests must pin an
  encrypted-content event's exact bytes and signature (mirroring `v2-signed-record-golden-vectors`).
- **Decryption is fold-read-time, best-effort.** A node that lacks `room_key[epoch]` for a
  content event treats the body as *unreadable*, not *invalid*: the event still validates,
  folds (if membership), and persists — it is simply not surfaced to the user. This must not
  wedge convergence or the fold.
- **AEAD failure = drop, never crash.** A tampered `ciphertext`/`nonce`/AAD fails AEAD open;
  the body is dropped (logged) and the event is treated as unreadable. No panic on peer bytes
  (spec §9 discipline, unchanged).
- **Replay of an old distribution record** is harmless: rotating *to* an already-known epoch
  is idempotent; rotating *backward* (lower epoch) is rejected.
- **Unknown `key_epoch`** (a content event from a future epoch the node has not yet received
  keys for) is unreadable until the distribution record arrives — convergent because the
  distribution is in the same causal history.

---

## 6. Security, privacy, reliability, performance, migration

**Security.**

- **Threat-model delta.** T27 moves from "Partial — honest transport only" to **Controlled**
  for the malicious-reader case (post-removal content is ciphertext under a key the removed
  device never receives). **Proposed new rows** (added at step 6, not yet in the threat
  model):
  - **T28 — epoch key store is a new local secret.** The `epoch → room_key` map grants read
    access to all room history it covers; it must be protected at least as strongly as the
    identity keys (ADR-0001 scope). Partial until storage encryption lands.
  - **T29 — rotation latency / admin-offline stall.** A removed device reads content until the
    removal + distribution batch propagates (compounds T18 availability), and rotation stalls
    entirely if the single admin is offline. Small-room/online-peer scoping; Partial.
  - **T30 — metadata leakage.** `event_type`, `sender_id`, `device_id`, timing, and the causal
    DAG remain cleartext; an observer learns event existence/type, not content. Accepted
    (matches §13.5 no-full-metadata-privacy).
- **Removed-device key retention.** A malicious removed device keeps every epoch key it
  already held; rotation only withholds *future* keys. Correct and sufficient for G1.

**Privacy.** Content bodies become confidential against non-key-holders (including a malicious
removed device and, at rest, a reader of `rooms.db` without the key store). Metadata (D2) is
unchanged.

**Reliability.** Rotation is causal and convergent (distribution in the DAG). Unreadable
content never blocks the fold or convergence (§5). Admin-offline stall (T29) is the main
availability caveat.

**Performance.** One AEAD op per content publish/read; one X25519+AEAD wrap per Active member
per rotation. At the v1 ceiling (N≤40, single admin) this is negligible. The key store is
O(epochs) — one 32-byte key per removal.

**Migration.** This is a wire-format addition: a new `Content::Encrypted` envelope and a new
`MemberKeyDistribution` record. Older peers cannot decrypt (they see unreadable content) and
must not reject the envelope outright — a mixed-version window needs a compatibility decision
(§10 OQ-6). At-rest `rooms.db` content becomes ciphertext for *new* events; existing plaintext
history is unchanged.

---

## 7. Implementation steps (ordered, phased)

1. **Design sign-off (this document).** Pin the wire schema (OQ-2), the wrap channel (OQ-3),
   and the threat-model delta (§6) *before* any crypto lands.
2. **Pure crypto crate.** X25519 conversion, HKDF, AEAD wrap/unwrap — deterministic, sans-IO,
   golden-vector tested (mirrors the v2-core purity discipline). OQ-1 decides new sibling vs.
   v2-core relaxation.
3. **Encrypted-content wire schema** in `iroh-rooms-core`: `Content::Encrypted` envelope +
   AAD binding + encrypt-on-publish / decrypt-on-read, preserving signature/`event_id`/fold.
4. **Rotation lifecycle:** admin key-gen on removal, `MemberKeyDistribution` fold record,
   per-epoch key store, join-time key wrap (D6).
5. **Tests:** golden vectors (encrypted event + distribution record), rotation convergence
   (two-node removal → post-removal content unreadable to the removed device), AEAD-failure
   no-panic, replay/backward-epoch rejection, join-time backlog gating.
6. **Threat-model sign-off:** T27 → Controlled, add T28/T29/T30; update
   `docs/security/threat-model.md` and the release-notes limitation list.

Each step is independently reviewable; do not start step 2 before step 1 sign-off.

---

## 8. Acceptance criteria

- **AC1.** A removed device that keeps a patched/malicious transport subscribed to the room
  gossip topic receives post-removal content **only as ciphertext it cannot decrypt** (no
  `room_key[new_epoch]`). Verified by a two-node removal test where the removed node's read of
  a post-removal `MessageText` yields *unreadable*, while remaining members read it.
- **AC2.** Signature verification, `event_id` derivation, dedup, causal ordering, and the
  membership fold are byte-for-byte unchanged for encrypted-content events (golden vectors).
- **AC3.** Membership events (incl. join bootstrap) work unchanged on plaintext.
- **AC4.** A newly-joined invitee reads membership immediately and backlog content only after
  receiving the join-time key wrap.
- **AC5.** AEAD failure / unknown epoch / backward-epoch inputs are dropped or unreadable,
  never a panic and never a fold wedge.
- **AC6.** Threat model updated (T27 → Controlled; T28/T29/T30 recorded) and the
  release-notes limitation list reflects the new posture.

---

## 9. Risks (with mitigations)

| Risk | Impact | Mitigation |
|---|---|---|
| Key store compromised | All covered room history readable | T28: protect ≥ identity keys; storage-encryption follow-up. |
| Admin offline at removal | Rotation stalls; new content uses old epoch | T29: documented v1 constraint; removal still revokes access (#196/#197). |
| Wire-schema churn breaks old peers | Mixed-version rooms fail to read each other | OQ-6: compatibility decision + version tag on the envelope (`/v1`). |
| Join-bootstrap UX regression (no instant backlog) | New member sees empty history until key wrap | D6 + OQ-5: wrap keys on join; surface "syncing" state in CLI. |
| Envelope/AAD transplant attack | Ciphertext moved to another event/room | D2: AAD binds ciphertext to the exact event prefix + room. |
| Backward-epoch rotation (replay) | Node rotates to an old key | §5: reject epoch < current. |

---

## 10. Open questions

- **OQ-1.** New sibling pure crate for wrap/unwrap vs. relaxing `iroh-rooms-v2-core`'s
  "no payload encryption" invariant? (Default: new sibling — keeps the v2-core tripwire.)
- **OQ-2.** Exact wire shapes: `Content::Encrypted { inner_type, key_epoch, nonce, ciphertext }`
  as a single variant, and `MemberKeyDistribution` as a fold-integrated `Content` variant vs.
  a side-channel record. (Default: single `Content::Encrypted`; fold-integrated distribution.)
- **OQ-3.** Key-wrap channel: static-static X25519 (admin static ↔ device) vs. ephemeral
  X25519 for recipient-forward-secrecy of the channel. (Default: ephemeral; static is the
  simpler fallback.)
- **OQ-4.** Admin-offline rotation: accept the v1 stall, or pre-stage a delegation/recovery
  path? (Default: accept + document for v1; revisit with multi-admin.)
- **OQ-5.** Should a new invitee get *all* historical epoch keys (full backlog) or only the
  current epoch (forward-only)? Full-history is the Matrix-like default but widens the
  join-time secret. (Default: all epochs, matching current "read full backlog" behavior.)
- **OQ-6.** Mixed-version rooms during rollout: do old peers reject `Content::Encrypted`
  (fail-closed) or treat it as unreadable (fail-open-but-readable-later)? (Default:
  unreadable, matching §5; needs a version tag for a future hard cutover.)

---

## 11. Traceability

- **#191** (this issue) — malicious-reader forward secrecy after removal.
- **T27** (`docs/security/threat-model.md`) — honest-transport revocation landed (#196/#197);
  this spec is the malicious-reader layer it deferred to.
- **PRD §13.4/§13.5** — key rotation is roadmap, not MVP; this is its first concrete plan.
- **#194 / #195** — terminal member-removal delivery + lockout bound (the access-revocation
  half that #191 complements with content confidentiality).
- **`gossip-overlay-events-fan-out.md` D5** — public topic id is a rendezvous, not a secret;
  why admission alone cannot provide content confidentiality.

---

## 12. Out of scope (explicit)

- Full group E2EE ratchet / per-message (MLS-style) forward secrecy.
- Metadata privacy beyond the current posture (event type/timing/DAG stay cleartext).
- Multi-admin rotation, admin-offline rotation mechanisms, compromised-admin defense.
- Encrypting membership events, blobs at rest, or `audit.ndjson`.
- Multi-device key recovery / recovery phrases (PRD §13.5 separate items).
