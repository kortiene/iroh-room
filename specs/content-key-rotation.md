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
- **Status:** **Steps 1–7 landed.** Step 7 (broader test matrix) is complete: multi-epoch
  rotation, multi-member rooms, adversarial file-share/pipe reads, backward-epoch replay
  rejection, cross-partition conflict convergence, multi-epoch/paginated/partial key
  history, voluntary-leave rotation, and poisoned-epoch restart durability are all covered
  (`sync_convergence.rs`, `engine_tests.rs`). Step 7 also surfaced and fixed a durability
  gap in the D5a resolution path (the winning key + `source_event_id` is now persisted, so
  a restart cannot reload a stale losing key). Step 8 (release-notes sign-off) is in
  progress. The distribution payload layout is fixed (OQ-2 resolved for step 6).

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

Closing that requires **forward secrecy**: rotating a room **content key** whenever an Active
member leaves the Active set (an admin `member.removed` or a voluntary `member.left`) so
post-departure content is unreadable to the departed device even if it receives the
ciphertext.

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

- **G1.** After a `member.removed(D)` or `member.left(D)` is folded, content events authored
  in subsequent epochs are unreadable to `D` even if `D` receives the ciphertext (forward
  secrecy, post-departure).
- **G2.** Membership events remain plaintext and the fold, join bootstrap, signature
  verification, dedup, and causal ordering are byte-for-byte unchanged.
- **G3.** For an admin `member.removed`, removal and rotation are a **single atomic fold
  transition** (the rotation payload rides in the removal event), so every honest member
  rotates to the new epoch at the same causal point and `D` is excluded — no intermediate
  state where the removal is folded but the new key is not yet durable. For a voluntary
  `member.left`, the departure folds immediately; the departed member is excluded from
  future epochs only once the admin authors a rotation-bearing event excluding them. v1
  emits that rotation **only on an explicit admin rotation/removal, never automatically**,
  so until the operator rotates the departed member keeps decrypting — an open-ended
  window, honestly recorded in T29 (not a bounded one).
- **G4.** A newly-joined invitee can read room backlog after a bounded, chunked join-time key
  transfer.
- **G5.** The threat model moves T27 from "Partial — honest transport only" to "Controlled"
  for the malicious-reader case, with the residual risks recorded as new rows.

**Non-goals** (see §12)

- Full group E2EE ratchet / per-message forward secrecy (MLS-style).
- Metadata privacy (an observer still learns event existence, type, timing, DAG, and body
  length).
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

A content event's body is replaced by an **encrypted-content envelope**. The envelope is a new
`Content` variant so the closed content registry (`Content::parse`, which rejects any unknown
key as `InvalidContent`) stays closed and the rollout can be made reader-first (D8):

```
Content::Encrypted {
    inner_type: EventType,   // the plaintext body's real type (MessageText / FileShared / …)
    key_epoch: u64,          // which room-content-key epoch encrypted this body
    suite: u8,               // cryptographic-suite identifier (D3) — 0x01 for SUITE_V1
    nonce: [u8; 12],         // AEAD nonce, unique per (key_epoch, event)
    ciphertext: Vec<u8>,     // AEAD(room_key[key_epoch], plaintext_cbor, aad) — bounded (D2a)
}
```

- `plaintext_cbor` is the deterministic-CBOR encoding of the original content body.
- **Associated data (AAD):** the event's cleartext signed prefix — `schema_version ‖ room_id ‖
  sender_id ‖ device_id ‖ event_type ‖ created_at ‖ prev_events ‖ inner_type ‖ key_epoch ‖
  suite` — binds the ciphertext to this exact event, type, epoch, and suite so it cannot be
  transplanted onto another event, room, type, or epoch.
  *Normative encoding (pinned at §7 step 4, resolving OQ-2):* `aad =
  ENCRYPTED_AAD_CONTEXT ‖ canonical-CBOR(array)` of those ten fields in that order, with
  `ENCRYPTED_AAD_CONTEXT = "iroh-rooms:content-aad:v1"`; a CBOR *array* (not the canonical
  map, which would re-sort keys) preserves the field order and self-delimits the
  variable-length `prev_events`. The nonce is deliberately absent (an authenticated AES-GCM
  cipher input already); the D5 key commitment is deliberately absent (GHASH is not
  key-committing regardless of AAD contents — commitments are enforced at key adoption,
  D5/D5a). See `docs/protocol.md` §6 for the byte layout and `event/encrypted.rs` for the
  golden-vector-frozen implementation.
- The eight signed fields, `event_id`, and the signature remain cleartext and computed over
  the **envelope** form (the event as transmitted). Signature verification, `event_id`
  derivation, dedup, and the fold all operate on the envelope unchanged; only a holder of
  `room_key[key_epoch]` can recover the body.
- **`inner_type` stays cleartext** (like `event_type`) for routing and the key-aware
  authorization reads in D9. An observer learns event existence, `inner_type`, and ciphertext
  **length** (D6 metadata, §6/T30).

#### D2a — Ciphertext and decrypted-body bounds

`ciphertext` is bounded so a single envelope still fits the 1 MiB wire frame
(`MAX_FRAME_BYTES`) and the per-type size invariants survive encryption:

- `ciphertext.len() ≤ inner_type`'s existing plaintext cap **+ AEAD tag** (16 bytes). Because
  AEAD preserves length, capping the ciphertext at the plaintext cap + tag keeps every
  per-type size invariant intact end-to-end.
- The recovered body, once decrypted, MUST pass the **original strict `Content::parse` for
  `inner_type`** (exact key set, byte/count caps, enums) before it is used for anything. A
  body that fails strict parsing after a successful AEAD open is treated as *unreadable*
  (dropped, logged) — **not** as a fold rejection: the DAG verdict must not depend on whether
  the local node holds the key, or convergence forks. See D2b.

#### D2b — Decrypted-content validation does not change the DAG verdict

A malicious **Active** key holder can encrypt a structurally invalid inner body (oversized
`message.text`, malformed `file.shared`). The DAG verdict (accept/park/reject) for an
`Encrypted` event is computed **only from the cleartext envelope and the shared fold inputs**
— never from the decrypted body — so every node reaches the same verdict regardless of key
possession. The decrypted body's only consumer is *surfacing* (display, authorization
projection), and it is gated independently:

1. AEAD open fails → body unreadable (§5).
2. AEAD opens but the recovered body fails strict `Content::parse(inner_type)` → body
   unreadable (§5); the event is still accepted/persisted per the envelope verdict.
3. Only a body that both opens and strictly parses is surfaced or fed to the key-aware
   authorization reads (D9).

This preserves the strict-validation trust boundary for *what users and the pipe/blob gates
act on*, without making convergence key-dependent.

### D3 — One pinned cryptographic suite (SUITE_V1)

To be interoperable, the design pins **one** suite; no algorithm is left to implementer
choice. `suite = 0x01` (`SUITE_V1`) means exactly:

| Primitive | Pinned choice |
|---|---|
| Key agreement (wrap) | **X25519** (RFC 7748), device Ed25519 keys converted to Montgomery (`curve25519-dalek`). Ephemeral sender key per wrap (D3a). |
| Wrap KDF | **HKDF-SHA-256** (RFC 5869), `salt = room_id`, `info = "iroh-rooms/content-key-wrap/v1" ‖ key_epoch ‖ recipient_device`. No salt-less shortcut. |
| Wrap AEAD | **AES-256-GCM** (key 32 B, nonce 12 B, tag 16 B), `aad = room_id ‖ key_epoch ‖ recipient_device`. |
| Content AEAD | **AES-256-GCM** (key 32 B = `room_key[epoch]`, nonce 12 B unique per `(key_epoch, event)`, tag 16 B), `aad` per D2. |
| Content key | `room_key[epoch]` = 32 bytes from `getrandom` (CSPRNG), generated by the admin at rotation. |
| Suite id | `0x01`; any other `suite` value is rejected at parse (fail-closed). |

HKDF-SHA-256 (add `hkdf`) and X25519 (add `x25519-dalek`) are new dependencies of the pure
crate only. Golden vectors (§7 step 2) pin a known-answer wrap + content encryption so two
implementations must agree byte-for-byte. Primitives live in a **pure, deterministic, sans-IO
crate** (same invariants as `iroh-rooms-v2-core`): no transports, no async, no storage.
Whether this is a new sibling crate or a relaxation of `iroh-rooms-v2-core`'s exclusion is
**OQ-1** (§10) — a new sibling keeps the v2-core purity tripwire intact and is the default
recommendation.

#### D3a — Ephemeral wrap channel

Each wrap uses a fresh ephemeral X25519 keypair from the admin; the ephemeral public key rides
in the `MemberKeyDistribution` record next to the wrapped key. This gives
recipient-forward-secrecy of the wrap *channel*: compromising the admin's long-term key later
does not retroactively unwrap past distributions. (Static-static is the simpler fallback, OQ-3.)

### D4 — Rotation is a single protocol transition, triggered by every Active→Removed change

v1 has a **single immutable admin** (`MembershipSnapshot::admin()`). The rotation trigger is
**any** transition that moves an Active key holder out of the Active set — an admin-authored
`member.removed(D)` **or** a voluntary `member.left(D)` (both land the subject in `Removed`).
A member that leaves voluntarily while retaining its old key and a patched gossip subscription
must be excluded from future epochs exactly like a removed member; otherwise it reads future
content indefinitely.

The `SyncEngine::publish` path accepts, persists, and fans out **one** event per call, so a
"same causal batch" of two separate publishes (removal, then distribution) provides **no
atomicity**: if the admin process or the second publish fails after the removal is
distributed, remaining peers have folded the removal but keep encrypting new content with the
old key the departed device still holds — defeating G3. Rotation is therefore made atomic by
**carrying the rotation in the departure transition itself**, not as a separate publish. The
mechanism differs by who authors the departure:

- **Admin-authored `member.removed`:** the removal event **must** carry the rotation payload
  (D5) — one event, one publish, one atomic fold transition. The admin wraps the new epoch's
  keys at removal time, so there is no intermediate state. A `member.removed` *without* a
  rotation payload is a pre-#191 (or non-rotating) event; peers treat it as "no rotation
  performed" and keep the old epoch (the honest pre-rotation posture).
- **Voluntary `member.left`:** the departing member authors the leave but **cannot** wrap keys
  (only the admin holds the wrap role). A leave therefore **cannot** carry the rotation
  itself. Instead, the leave folds the departure immediately (access revocation via #196/#197
  applies at once), and the departed member is excluded from future epochs only when the admin
  next authors a rotation-bearing event (a `member.removed` with a rotation payload, or a
  dedicated `member.key_distribution`). Until that operator-driven rotation lands, new content
  is still encrypted under the old epoch the departed member holds. **v1 emits no automatic
  rotation**: an ordinary admin content publish does *not* rotate, so this window is
  open-ended — bounded only by when an operator rotates, recorded honestly in T29. The
  departee cannot force earlier rotation; that would require delegating wrap authority, which
  is out of scope (§12).

So "atomic" (G3) is precise for **admin removals** (single event), and **open-ended** for
**voluntary leaves** (departure folds immediately; key exclusion lands only at an explicit
admin rotation, which v1 never fires automatically). Both are stated without claiming more
than the mechanism provides.

*Why not a fail-closed intermediate state instead?* An alternative — block old-epoch
publishing until the distribution is durable — adds a liveness dependency and a new failure
mode (stalled rooms) without closing the gap better than embedding the rotation in the
admin's departure/rotation event. Embedding is simpler and keeps one event per transition for
the common (admin-removal) case. Recorded as the considered alternative; embedding chosen
(OQ-7 confirms).

*Admin-offline stall:* if the admin is offline when a removal is needed, rotation (and thus
forward secrecy) stalls until the admin returns — the removal still revokes *access* via
#196/#197, but new content keeps using the old epoch. The same stall applies to the
post-leave rotation above. Accepted v1 constraints given the single-admin model; §6/T29,
§10 OQ-4.

### D5 — `MemberKeyDistribution` rides in an admin-authored event, with a key commitment

The distribution payload is always **admin-authored** (only the admin wraps keys): embedded in
the admin's `member.removed` (atomic with the removal, D4) or in the admin's next event after
a voluntary `member.left`. Being in the causal DAG, it is fold-visible at the same causal
point on every node. It carries:

- **Cleartext envelope:** `new_epoch`, a **key commitment** `BLAKE3(room_key[new_epoch])`, the
  admin's ephemeral wrap public key (D3a), and `prev_events` (part of the departure event).
- **Per-recipient ciphertext:** a map `device_id → wrapped_key` (D3). A recipient opens only
  its own entry.

Every node learns `new_epoch` and the **commitment** (but not the key) from the cleartext
envelope; only wrapped recipients recover `room_key[new_epoch]` and can verify it against the
commitment. The removed/departed device is excluded from the wrapped set, so it learns the
commitment but never the key.

#### D5a — Conflicting distributions for the same epoch fail closed

Concurrent admin departures can legitimately fork from the same prior head and choose the same
incremented `new_epoch` while generating **different** random keys (the fold currently flags
admin equivocation but does not structurally prevent it). Treating the second same-epoch
distribution as idempotent would make the chosen key arrival-order-dependent, so different
peers would decrypt different branches and convergence is lost. The rule is therefore:

- A distribution for an epoch the node already has is **idempotent only if its key commitment
  matches** the already-accepted one (bit-for-bit). Re-seeing the same commitment is a no-op.
- A distribution for an already-accepted epoch with a **different** commitment is a
  **fail-closed conflict**: the node refuses to adopt either key for that epoch, surfaces a
  loud admin-equivocation error, and treats new content in that epoch as unreadable until the
  fork is resolved by the deterministic fork-resolution rule (the existing admin-chain fork
  handling picks one branch; the winning branch's commitment becomes the epoch's key). This is
  deterministic and convergent, never arrival-order-dependent.

### D6 — Join-time key transfer is chunked and bounded

A newly-joined invitee needs current + historical epoch keys to read backlog. The full
`epoch → room_key` map grows without a protocol bound, and the sync contract caps a wire frame
at 1 MiB (`MAX_FRAME_BYTES`) with `SyncEngine::publish` rejecting any single event that cannot
fit — so a long-lived, high-churn room would eventually be unable to add a member. The
join-time transfer is therefore **chunked** exactly like ordinary membership history:

- The admin serves the invitee's key history as a **sequence of bounded
  `MemberKeyDistribution`-style records** (each carrying at most `KEY_HISTORY_CHUNK` epochs —
  sized so one record + overhead fits well under 1 MiB), riding the same chunked-pull path as
  the membership sub-DAG, not a single monolithic publish.
- **Bounded-history alternative (OQ-5):** cap how many historical epochs a new invitee receives
  (forward-only from `current_epoch − KEY_HISTORY_WINDOW`). This bounds the transfer to a
  constant but changes "read full backlog" to "read recent backlog." The default keeps full
  history via chunking; OQ-5 decides whether a window is also offered.

Until the join-time transfer completes, the invitee reads membership (plaintext) but not
historical content — a deliberate UX change from "read backlog immediately" (§6, §10 OQ-5).

### D7 — Old epochs stay decryptable; forward secrecy is post-removal only

A removed/departed device already had access to content up to its departure; rotating keys
does not (and cannot) make that history unreadable, and that is **not** a goal. The per-node
key store is a small `epoch → room_key` map retained for reading backlog. Forward secrecy is
*post-departure*: only content authored in epochs after `D`'s departure is unreadable to `D`.

### D8 — Rollout is reader-first; writers enable only after a compatibility floor

`SignedEvent::from_canonical_value` runs the closed, strict `Content::parse`, whose per-type
parsers reject unknown keys as `InvalidContent`. A peer that predates `Content::Encrypted`
therefore does **not** see the envelope as merely unreadable — it **rejects the event
entirely** and can park its causal descendants as missing, partitioning the room. Rollout
must therefore be reader-first:

1. **Phase R1 (reader):** ship a release whose `Content::parse` recognizes the
   `Content::Encrypted` envelope as an **opaque, valid-but-unreadable** variant (parses and
   persists it, surfaces nothing), with **writers disabled**. No encrypted event is emitted
   yet, but every upgraded peer tolerates one.
2. **Phase R2 (writer):** after a declared compatibility floor (a version string / room
   capability flag proving all Active peers are ≥ R1), writers begin emitting
   `Content::Encrypted`. A room opts in explicitly; mixed rooms without the floor keep
   plaintext. *v1 mechanism (§7 step 4):* the floor is an operator-asserted per-engine
   declaration (`SyncConfig::encrypted_content_writes`, default off and not yet exposed to
   CLI/net configuration); while it is on, locally-authored **plaintext** content-class
   publishes are refused fail-closed so the opt-in cannot leak cleartext by accident. The
   room-scoped capability/opt-in surface lands with the rotation lifecycle (§7 step 6).
3. **Hard cutover (optional, later):** a schema-version bump can make encrypted the default
   for new rooms once the ecosystem is past R1. Recorded as OQ-6.

### D9 — Pipe/blob authorization reads become key-aware

Encrypting `FileShared`, `PipeOpened`, and `PipeClosed` replaces the concrete `Content`
variants that authorization helpers inspect directly: `SyncEngine::file_shared_hashes`,
`SyncEngine::pipe_opened`, and `SyncEngine::pipe_is_closed` currently match the plaintext
variants. These reads are **authorization decisions** (blob fetch ACL, pipe open/close state)
and must not silently break: without them, blob fetches are denied, pipe opens disappear, and
an encrypted `PipeClosed` is missed — potentially leaving a closed pipe authorized
(fail-open). The implementation therefore converts these reads to **authenticated
decryption**:

- Each helper (`file_shared_hashes`, `pipe_opened`, `pipe_is_closed`) gains an encrypted path:
  on an `Encrypted` event with the matching `inner_type`, attempt AEAD open + strict
  `Content::parse(inner_type)` (D2b); only a body that passes both feeds the projection.
- **Fail-closed, never fail-open:** if the local node lacks the epoch key or decryption/parse
  fails, the event contributes **nothing** to the authorization projection (a blob hash is not
  added, a pipe is not marked open, a close is honored only when a *readable* close is seen —
  and an unreadable close is treated conservatively as still-closed, not reopened).
- The implementation plan (§7) includes these helpers and their fail-closed tests explicitly,
  not only `MessageText` rotation.

---

## 5. Validation, authorization, and error model

- **Signature/CSB invariants preserved.** `event_id = BLAKE3(CSB)` and the Ed25519 signature
  are computed over the transmitted (envelope) event. Golden-vector tests must pin an
  encrypted-content event's exact bytes and signature (mirroring `v2-signed-record-golden-vectors`).
- **Envelope validation is key-independent.** The DAG verdict (accept/park/reject) for an
  `Encrypted` event is computed only from the cleartext envelope + shared fold inputs, never
  from the decrypted body (D2b), so every node converges on the same verdict regardless of key
  possession.
- **Decryption is fold-read-time, best-effort, and strictly parsed.** A node that lacks
  `room_key[epoch]`, fails AEAD open, or recovers a body that fails strict
  `Content::parse(inner_type)` treats the body as *unreadable* (dropped, logged), never
  *invalid*: the event still validates and persists per the envelope verdict — it is simply
  not surfaced and feeds nothing into the key-aware authorization projections (D9). This must
  not wedge convergence or the fold, and never panics on peer bytes (spec §9 discipline).
- **Ciphertext is bounded.** `ciphertext.len() ≤ inner_type`'s plaintext cap + 16-byte tag
  (D2a); an oversized ciphertext is rejected at envelope parse (fail-closed), keeping the
  1 MiB wire frame and per-type size invariants intact.
- **Same-epoch distribution conflict fails closed.** A repeated `new_epoch` distribution is
  idempotent only when its key commitment matches bit-for-bit; a conflicting commitment is a
  fail-closed admin-equivocation error resolved deterministically by the admin-chain fork
  rule, never by arrival order (D5a).
- **Backward-epoch distribution is rejected.** Rotating to a lower or already-superseded epoch
  is a protocol violation and ignored (logged).
- **Unknown future `key_epoch`** (content from an epoch the node has not yet received keys
  for) is unreadable until the distribution for it folds — convergent because the distribution
  rides in the causal DAG in an admin-authored event (D4/D5).

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
  - **T30 — metadata leakage (incl. length).** `event_type`/`inner_type`, `sender_id`,
    `device_id`, timing, the causal DAG, **and the deterministic-CBOR body's length** (AEAD
    preserves plaintext length apart from the 16-byte tag) remain cleartext. An observer or
    removed device learns event existence, type, and an often-effectively-exact body size —
    enough to gauge message length, distinguish short status updates, and aid traffic
    correlation. Accepted (matches §13.5 no-full-metadata-privacy); padding to hide length is
    **not** in v1 scope (§12) and would be a separate decision if ever wanted.
- **Removed-device key retention.** A malicious removed device keeps every epoch key it
  already held; rotation only withholds *future* keys. Correct and sufficient for G1.

**Privacy.** Content bodies become confidential against non-key-holders (including a malicious
removed device and, at rest, a reader of `rooms.db` without the key store). Metadata (D2) is
unchanged.

**Reliability.** An admin removal and its rotation are a single atomic fold transition
(rotation embedded in the removal event, D4), so there is no intermediate state where the
removal is folded but the key is not yet durable. Unreadable content never blocks the fold or
convergence (§5). Admin-offline stall (T29) and the bounded voluntary-leave rotation window
are the main availability
caveats.

**Performance.** One AEAD op per content publish/read; one ephemeral-X25519 + AEAD wrap per
remaining Active member per departure. At the v1 ceiling (N≤40, single admin) this is
negligible. The key store is O(epochs) — one 32-byte key per departure; the join-time transfer
is chunked (D6) so a long-lived room stays joinable.

**Migration.** This is a wire-format addition: a new `Content::Encrypted` envelope and the
departure-embedded `MemberKeyDistribution` payload. Rollout is **reader-first** (D8): a reader
release that parses the envelope as opaque-but-valid lands **before** any writer emits it,
with an explicit compatibility floor (room capability flag) gating writers. Without R1-first,
pre-`Encrypted` peers reject the event entirely and the room partitions. At-rest `rooms.db`
content becomes ciphertext for *new* events; existing plaintext history is unchanged.

---

## 7. Implementation steps (ordered, phased)

1. **Design sign-off (this document).** Pin the wire schema (OQ-2), the crypto suite (D3 —
   SUITE_V1 is pinned here, not open), the wrap channel (OQ-3), the rotation-atomicity
   mechanism (D4/OQ-7), and the threat-model delta (§6) *before* any crypto lands.
2. **Pure crypto crate.** X25519 conversion, HKDF-SHA-256, AES-256-GCM wrap/unwrap,
   ephemeral-sender wrap channel (D3a) — deterministic, sans-IO, golden-vector tested
   (mirrors the v2-core purity discipline). OQ-1 decides new sibling vs. v2-core relaxation.
3. **Reader-first envelope (Phase R1, D8).** `Content::parse` recognizes `Content::Encrypted`
   as an opaque valid-but-unreadable variant with **writers disabled**, plus the D2a
   ciphertext bound. Lands before any writer.
4. **Encrypted-content write path (Phase R2, D8).** `Content::Encrypted` envelope + AAD
   binding + encrypt-on-publish / decrypt-on-read with D2b post-decryption strict parse,
   preserving signature/`event_id`/fold, gated behind the room compatibility floor.
5. **Key-aware authorization reads (D9).** Convert `SyncEngine::file_shared_hashes`,
   `pipe_opened`, `pipe_is_closed` to authenticated decryption with fail-closed semantics +
   their tests.
6. **Rotation lifecycle (D4/D5/D6):** rotation payload embedded in `member.removed` /
    `member.left` (single publish), key commitment + same-epoch conflict fail-closed (D5a),
    per-epoch key store, chunked join-time key transfer (D6). **(Landed: `member.removed`
    rotation payload + standalone `member.key_distribution` event, D5a deterministic
    resolution by smallest `event_id`, `room_keys` persistence table, `WantKeyHistory` /
    `KeyHistory` sync messages, engine handlers for serving and adopting key history.)**
7. **Tests:** golden vectors (SUITE_V1 wrap + encrypted event + distribution payload), rotation
    convergence (two-node removal → post-removal content unreadable to the removed device),
    D2b malicious-inner-body (valid DAG verdict, body unreadable), AEAD-failure no-panic,
    same-epoch conflict fail-closed, backward-epoch rejection, join-time chunked transfer,
    pipe/blob fail-closed reads. **(Landed; broadened to the full step-7 matrix: multi-epoch
    rotation, multi-member rooms, adversarial file-share/pipe ciphertext-only reads,
    backward-epoch replay rejection, cross-partition D5a convergence, multi-epoch /
    paginated / partial key-history transfers, voluntary-`member.left` rotation, and
    poisoned-epoch restart durability. Step 7 also fixed the D5a resolution path to persist
    the winning key + `source_event_id`.)**
8. **Threat-model sign-off:** T27 → Controlled, add T28/T29/T30; update
    `docs/security/threat-model.md` and the release-notes limitation list. **(Threat-model
    rows landed at step 6; release-notes limitation list lands here.)**

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
  the chunked join-time key transfer completes; a long-lived room stays joinable within the
  1 MiB frame bound.
- **AC5.** AEAD failure / unknown epoch / backward-epoch inputs are dropped or unreadable,
  never a panic and never a fold wedge.
- **AC6.** Threat model updated (T27 → Controlled; T28/T29/T30 recorded) and the
  release-notes limitation list reflects the new posture.
- **AC7.** A voluntary `member.left` folds the departure immediately (access revoked at once),
  and an admin-authored rotation event then excludes the departed member: from that fold point
  the departed member receives no new-epoch key. v1 does **not** rotate automatically, so the
  window between leave and the operator-driven rotation is open-ended and honestly recorded
  (T29), not bounded by the protocol.
- **AC8.** A malicious Active key holder's encrypted-but-invalid inner body (oversized
  `message.text`, malformed `file.shared`) yields the **same** DAG verdict on every node
  (key-independent), is persisted, and is surfaced as unreadable — never a fold wedge and
  never a per-type validation bypass.
- **AC9.** Pipe/blob authorization reads (`file_shared_hashes`, `pipe_opened`,
  `pipe_is_closed`) are key-aware and fail-closed: an encrypted `PipeClosed` is honored, an
  unreadable one never reopens a closed pipe, and blob hashes from unreadable `FileShared`
  events are not served.
- **AC10.** Rollout is reader-first: a pre-writer peer parses `Content::Encrypted` as
  opaque-but-valid (no event rejection, no room partition); writers emit only behind the room
  compatibility floor.

---

## 9. Risks (with mitigations)

| Risk | Impact | Mitigation |
|---|---|---|
| Key store compromised | All covered room history readable | T28: protect ≥ identity keys; storage-encryption follow-up. |
| Admin offline at removal | Rotation stalls; new content uses old epoch | T29: documented v1 constraint; removal still revokes access (#196/#197). |
| Voluntary leave without prompt rotation | Departed member reads content until the admin rotates | D4: leave folds immediately (access revoked); the departed member keeps its old epoch key until an operator-driven rotation excludes it. Open-ended window — no automatic rotation in v1 (T29). |
| Non-atomic removal+rotation | Peers fold removal, keep encrypting old-epoch | D4: rotation embedded in the departure event (single publish), not a second publish. |
| Pre-writer peers reject envelope | Room partitions on mixed versions | D8: reader-first rollout + room capability floor before writers. |
| Malicious Active key holder encrypts invalid body | Per-type validation bypass | D2b: key-independent DAG verdict + post-decryption strict parse → unreadable. |
| Same-epoch conflicting keys (admin fork) | Peers decrypt different branches | D5a: commitment-match idempotence; conflict fails closed, resolved by fork rule. |
| Join-time key history exceeds frame | Long-lived room becomes unjoinable | D6: chunked join-time transfer (or OQ-5 bounded window). |
| Envelope/AAD transplant attack | Ciphertext moved to another event/room | D2: AAD binds ciphertext to the exact event prefix + room + epoch + suite. |
| Backward-epoch rotation (replay) | Node rotates to an old key | §5: reject epoch < current. |

---

## 10. Open questions

The crypto suite (SUITE_V1, D3) is **pinned**, not open. These remain for build time:

- **OQ-1.** New sibling pure crate for wrap/unwrap vs. relaxing `iroh-rooms-v2-core`'s
  "no payload encryption" invariant? (Default: new sibling — keeps the v2-core tripwire.)
- **OQ-2.** Exact field-level wire shapes of `Content::Encrypted` and the distribution payload
  (field names/order, `inner_type` encoding). The *structure* is decided (single
  `Content::Encrypted` variant; rotation in an admin-authored event); only the concrete field
  layout remains. *Resolved for the envelope + AAD:* the envelope field layout was frozen by
  the §7 step 3 golden vectors, and the normative AAD encoding by step 4 (see D2). *Resolved
  for the distribution payload:* the `MemberKeyDistribution` payload layout was frozen by the
  step 6 golden vectors (`new_epoch`, `key_commitment`, canonically-sorted `wrapped_keys`
  array of `[device_id, {ephemeral_public, nonce, ciphertext}]` pairs).
- **OQ-2b.** Where the post-`member.left` rotation lives: a dedicated admin rotation event vs.
  piggybacked on the admin's next ordinary event. (Default: a dedicated rotation event for
  clarity; piggybacking is the optimization.)
- **OQ-3.** Key-wrap channel: ephemeral X25519 (D3a, default) vs. static-static (simpler,
  loses channel forward-secrecy). (Default: ephemeral.)
- **OQ-4.** Admin-offline rotation: accept the v1 stall, or pre-stage a delegation/recovery
  path? (Default: accept + document for v1; revisit with multi-admin.)
- **OQ-5.** Join-history breadth: full history via chunked transfer (D6, default) vs. also
  offering a bounded `KEY_HISTORY_WINDOW` (forward-only from `current_epoch − window`). Full
  history matches current "read full backlog" but widens the join-time secret.
- **OQ-6.** Hard cutover: after the reader-first floor (D8), does a future schema-version bump
  make encrypted the default for *new* rooms? (Default: keep opt-in per room for v1.)
- **OQ-7.** Confirm rotation-atomicity mechanism for admin removals: embedding the rotation
  payload in the removal event (D4, chosen) vs. a fail-closed intermediate that blocks
  old-epoch publish until the distribution is durable. Embedding is simpler and adds no
  liveness dependency; confirm at sign-off.

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
- Metadata privacy beyond the current posture: event existence, `event_type`/`inner_type`,
  timing, the causal DAG, and **ciphertext length** stay cleartext. **Length-hiding padding
  is not in v1 scope** (would be a separate decision; see T30).
- Multi-admin rotation, admin-offline rotation mechanisms, compromised-admin defense.
- Encrypting membership events, blobs at rest, or `audit.ndjson`.
- Multi-device key recovery / recovery phrases (PRD §13.5 separate items).
