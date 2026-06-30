# Spec: Key-Bound Invite Ticket Generation (IR-0103)

- **Issue:** #18 — `[IR-0103] Implement key-bound invite ticket generation`
- **Parent epic:** #2
- **Labels:** type/feature, type/security, area/protocol, area/cli, priority/p0, risk/high
- **Depends on:** #17 (room create + store wiring, **landed**), #12 (membership fold, **landed**)
- **Status:** landed — implemented in issue #18 / IR-0103; this document is the build plan.
- **Traceability:** PRD `PRD.v0.3.md` §10.5, §13.1, §15.3; `PHASE-0-SPIKE.md` Event
  Protocol §7 (`member.invited`), Membership & Ordering §3.5 / §3.7 / §6.

---

## 1. Summary

Add the admin-only command

```text
iroh-rooms [--data-dir <PATH>] room invite <ROOM_ID> --invitee <IDENTITY_ID> [--role member|agent] [--expires <DURATION>]
```

which:

1. confirms the caller is the room's single immutable admin (AC1),
2. mints a fresh `invite_id` (16 B) and a fresh **capability secret** (16 B) from the OS CSPRNG,
3. computes `capability_hash = BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)`,
4. assembles, signs, self-validates, and **persists** an admin-signed `member.invited`
   event whose content carries the *hash* (never the secret) and is **bound to the
   invitee identity key** (AC2, AC3),
5. emits an **out-of-band invite ticket** — a copy-pasteable text token carrying the
   room id, invite id, capability **secret**, invitee key, optional expiry, and a
   discovery hint (the admin's `device_id` / `EndpointId`),
6. prints the ticket and its expiry clearly, with a "treat like a password" warning (AC5).

The protocol substrate for this feature **already exists** and is the reason risk is
contained:

- `Content::MemberInvited` and its strict CBOR parser/encoder live in
  `iroh-rooms-core::event::content` (`crates/iroh-rooms-core/src/event/content.rs`).
- `capability_hash(room_id, invite_id, secret)` is implemented and unit-covered in the
  same module.
- The membership fold already **gates `member.invited` as admin-only**
  (`gate_admin_action`) and already **consumes a key-bound invite** in the join gate
  (`gate_join`), including log-only expiry and sticky-departure
  (`crates/iroh-rooms-core/src/membership/fold.rs`).

So IR-0103 is **not** new protocol. It is three thin, well-bounded additions on top of
landed primitives:

- **(core builder)** a pure `build_member_invited(...)` assembler, mirroring the landed
  `build_room_created` (`event::genesis`);
- **(core ticket)** a `RoomInviteTicket` value type with a canonical, round-trippable
  text encoding;
- **(cli)** a `room invite` orchestration that does the RNG + admin check + persist +
  ticket emission, mirroring the landed `room create` orchestration (`cli/src/room.rs`).

The **joiner side** (`member.joined`, parsing the ticket, proving the capability) is a
**sibling issue** and is explicitly out of scope here (see §3.2).

---

## 2. Background & current repository state

### 2.1 What exists (landed work this builds on)

- **Canonical signed event model** (`event`, IR-0002): `SignedEvent`, CSB encoding,
  `WireEvent::seal`, `sign_csb`, the stateless `validate_wire_bytes` pipeline, and the
  strict `Content::MemberInvited` parser/encoder. The `member.invited` content schema is
  *already finalized*:
  ```rust
  pub struct MemberInvited {
      pub invite_id: [u8; 16],
      pub capability_hash: [u8; 32],
      pub role: String,                 // "member" | "agent" | "admin"
      pub invitee_key: IdentityKey,     // key-bound (REQUIRED)
      pub expires_at: Option<u64>,      // ms epoch; absent ⇒ no expiry
      pub invitee_hint: Option<String>, // non-authoritative label
  }
  ```
- **`capability_hash`** (`event::content::capability_hash`): the exact §7 derivation,
  domain-separated by `INVITE_CONTEXT = b"iroh-rooms:invite:v1"`
  (`event::constants`).
- **SQLite event store** (`store`, IR-0004): `EventStore::open/insert/get`,
  `room_event_ids`, and **`heads(room)`** — the DAG heads this event must cite as
  `prev_events`.
- **Membership fold** (`membership`, IR-0008): `RoomMembership::from_events(...).snapshot()`
  yields the admin and per-identity status/role. The fold's gate already:
  - accepts `member.invited` **iff** `signer == admin` (`gate_admin_action`);
  - in `gate_join`, requires a *naming, key-bound, capability-matching, role-matching,
    unexpired, un-consumed* admin invite in the join's causal ancestors;
  - enforces **log-only expiry** (`join.created_at > invite.expires_at ⇒ ExpiredInvite`)
    and **sticky departure** (a removal/leave consumes the invite).
- **Identity & room CLI** (IR-0101 / IR-0102): `identity::{Profile, SecretKeys}`,
  `paths::data_dir`, `clock::now_ms`, and the `room::{create, members}` orchestration —
  the precise pattern this feature copies. The store already lives at `<HOME>/rooms.db`
  and holds many rooms keyed by `room_id`.

### 2.2 The real gap (this issue closes it)

There is **no way to author a `member.invited` event or produce a ticket**. The fold can
*validate* and *consume* an invite, but nothing *mints* one. `room create` is the only
authoring command; `room members` is the only read command. IR-0103 adds the second
authoring command (the invite) and the first out-of-band capability artifact (the ticket).

### 2.3 Spike / PRD facts that constrain the design

- **Key-bound only (path-A).** `member.invited.invitee_key` is **required**; open/bearer
  tickets are excluded from MVP because they break removal semantics (Spike §6;
  issue Security note). The CLI therefore **requires `--invitee`**.
- **The secret never touches the log.** `capability_hash` is on the event; the secret
  travels **only** inside the ticket (Spike §6, §7). AC3.
- **Capability hash.** `capability_hash = BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)`,
  secret = exactly 16 bytes (matches `member.joined.capability_secret: bstr[16]`).
- **Expiry is log-only & advisory-clock-free.** Validity is
  `expires_at` absent **OR** `join.created_at <= expires_at`; both are signed, so every
  peer computes the same verdict. The local clock is **never** an authorization input
  (Spike §6 "Expiry determinism"). The invite command only *encodes* `expires_at`; the
  enforcement already lives in `gate_join`.
- **`prev_events` = room heads.** `member.invited` is a non-genesis event signed by the
  admin's **device** key; it cites the current room DAG heads (Spike §7 table).
- **Single immutable admin.** Exactly one admin per room (the creator). Only that key may
  invite (Spike §3.5, §7). In MVP a non-admin member who runs `room invite` must be
  rejected up front.
- **No native revocation (documented MVP limitation).** The only way to undo an invite is
  to remove the subject; `max_uses` is not convergently enforceable, so a key-bound invite
  is single-subject and reusable by that key until expiry (Spike §6 "MVP limitations").

### 2.4 Workspace conventions to honor

- **Pure, deterministic core assemblers**: builders take injected RNG/clock outputs and
  contain no wall-clock or RNG (mirror `build_room_created`); the only RNG in `core` stays
  inside `SigningKey::generate`.
- **Validate-before-persist**: the CLI self-validates a freshly built event through the
  real pipeline and only writes on success (mirror `room::create`).
- **Secret hygiene**: signing secrets live in `Zeroizing`/`SigningKey` wrappers and never
  appear in any output or error path; no `Debug`/`Display` on secret-bearing types.
- **Pre-IO validation**: argument validation runs before any filesystem write, so a bad
  invocation leaves the store untouched.
- **Errors → stderr + non-zero exit; success → stdout + exit 0.**
- **`scripts/verify.sh`** (fmt + clippy `-D warnings` + workspace tests) is the gate.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A pure core builder `build_member_invited(...)` that assembles + signs a
   `member.invited` event from injected `invite_id`, `capability_hash`, `role`,
   `invitee_key`, `expires_at`, `invitee_hint`, `prev_events`, and `created_at`.
2. A core `RoomInviteTicket` value type + canonical text encoding (`Display`/`FromStr`)
   carrying room id, invite id, capability **secret**, invitee key, role, optional
   `expires_at`, and a discovery hint (admin `device_id`).
3. Re-export `capability_hash` at `event::` for ergonomic CLI use.
4. The `room invite` CLI orchestration: admin check, CSPRNG `invite_id`/secret, hash,
   `--expires` duration parsing, build + self-validate + persist, ticket emission.
5. `--invitee <IDENTITY_ID>` (required), `--role member|agent` (optional, default
   `member`), `--expires <DURATION>` (optional).
6. Clear, script-friendly output: the ticket, the bound invitee key, the expiry (absolute
   + human), and a password-grade warning.
7. Unit + CLI tests for: admin invite happy path, non-admin rejection, secret-absent-from-log,
   capability-hash-recomputes-from-ticket, and expired-invite behavior (§11).

### 3.2 Out of scope (sibling issues — do **not** implement here)

- **`member.joined` / `room join`** (parse a ticket, prove the capability, bind the
  joiner device). The fold's `gate_join` already exists; the CLI join flow is a sibling
  under #2.
- **Network push / live broadcast** of the invite. `room invite` is an **offline, local**
  command (like `room create`); the event is persisted and propagates later via the
  sync/net layers (IR-0007 / IR-0005). No endpoint is brought up.
- **Invite revocation, `max_uses`, open/bearer tickets, key rotation** (PRD §13.4/§13.5;
  Spike §6 — post-MVP).
- **Agent-specific invite ergonomics** beyond `--role agent` (PRD §13.3 agent flow may
  refine this later).
- **Rich `NodeAddr` discovery hints** (relay URL + direct socket addrs). MVP carries the
  admin `device_id` (`EndpointId`) only; iroh n0 discovery resolves it. See OQ3.

### 3.3 Why the split is safe

The trust boundary (admin-only authoring, key-binding, capability matching, expiry,
sticky departure) is **already enforced deterministically by the landed fold and stateless
validator**. This issue only *produces* a well-formed, admin-signed event and an
out-of-band secret carrier. Even a buggy ticket cannot grant access: a join is authorized
solely by the on-log `member.invited` + a secret that recomputes the on-log
`capability_hash`. A malformed or leaked ticket fails the join gate on every peer.

---

## 4. Key design decisions

### D1 — Pure `member.invited` builder in core (recommended)

Add `crates/iroh-rooms-core/src/event/invite.rs` exporting `build_member_invited`,
re-exported as `event::build_member_invited` (sibling to `event::genesis::build_room_created`).

```rust
/// Assemble and sign an admin-issued `member.invited` event (Event Protocol §7).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `invite_id`, `capability_hash` (i.e. the secret draw), `expires_at`, `prev_events`,
/// and `created_at` are injected by the caller so this stays free of wall-clock and RNG.
///
/// `member.invited` carries **no** embedded `device_binding`: it is a
/// membership-device-bound type (`requires_membership_device_binding == true`), so the
/// admin's device is resolved from the genesis binding by the fold. The event is signed
/// by the admin's **device** secret; the signature MUST verify under `device_id`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_member_invited(
    admin_identity_secret: &SigningKey, // provides sender_id (== admin identity)
    admin_device_secret: &SigningKey,   // signs the event; device_id
    room_id: &RoomId,
    invite_id: &[u8; SHORT_ID_LEN],
    capability_hash: &[u8; DIGEST_LEN],
    role: &str,
    invitee_key: &IdentityKey,
    expires_at: Option<u64>,
    invitee_hint: Option<&str>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent
```

Rationale: matches the landed builder pattern exactly (one conformance test, reused by the
CLI and by any future flow), keeps RNG/clock out of `core`, and is golden-testable. The
builder does **not** generate the secret or the hash — that is the caller's RNG concern
(D2) — it accepts the already-computed `capability_hash`.

Alternative (rejected): assemble the event inline in the CLI. Rejected because it would
duplicate CSB/sign/seal logic and lose the single golden-vector test the builder gives.

### D2 — RNG (secret + invite_id) and the hash live in the CLI

The 16-byte `capability_secret` and 16-byte `invite_id` are drawn with
`getrandom::fill` in the CLI (the same CSPRNG `room create` uses for `room_nonce`). The
CLI computes `capability_hash` via the re-exported `event::content::capability_hash`
(D3). The secret is held in a `Zeroizing<[u8; 16]>` from draw until it is encoded into the
ticket string; it is **never** placed in the event content and never logged.

Rationale: keeps `core` RNG-free; localizes the one secret-bearing buffer; mirrors the
`room_nonce` handling in `room::create`.

### D3 — Re-export `capability_hash` at `event::`

`capability_hash` is already `pub` in `event::content` but not re-exported. Add
`pub use content::capability_hash;` to `event/mod.rs` so the CLI calls
`iroh_rooms_core::event::capability_hash(...)` rather than reaching into a submodule.
No behavior change.

### D4 — `RoomInviteTicket` value type + canonical encoding, in core (recommended)

Add `crates/iroh-rooms-core/src/ticket.rs` (new top-level module, sibling to `membership`,
`store`, `sync`), exporting:

```rust
pub struct RoomInviteTicket {
    pub room_id: RoomId,
    pub invite_id: [u8; SHORT_ID_LEN],
    pub capability_secret: [u8; SHORT_ID_LEN], // the out-of-band secret
    pub invitee_key: IdentityKey,
    pub role: String,
    pub expires_at: Option<u64>,
    pub inviter_identity: IdentityKey,         // admin sender_id (provenance)
    pub discovery: Vec<DeviceKey>,             // discovery hints; MVP = [admin device_id]
}

impl RoomInviteTicket {
    pub fn capability_hash(&self) -> [u8; DIGEST_LEN]; // recompute against room_id+invite_id+secret
}
impl fmt::Display for RoomInviteTicket { /* canonical text token */ }
impl FromStr for RoomInviteTicket { type Err = TicketError; /* round-trips Display */ }
```

Place the ticket in **core** (not the CLI) so the sibling `room join` flow can decode it
without duplicating the codec, exactly as the builder is shared. The ticket type is a
plain value object — it carries a secret and **must not** derive `Debug`/`Display` of the
secret bytes beyond the deliberate `Display` encoding; provide a redacted `Debug` that
masks `capability_secret`.

**Encoding** (decision, recommended): a self-describing, single-line token

```text
roomtkt1<base32-lowercase-nopad( version(1B) ‖ canonical-CBOR(body) ‖ blake3_checksum(4B) )>
```

- Body is the deterministic-CBOR map of the fields above, reusing the **landed core
  codec** (`event::cbor::{encode, decode_canonical, CborValue}`) — no new CBOR dependency,
  and the same canonical profile the rest of the protocol uses.
- `version = 1` allows forward-compatible format changes.
- A 4-byte BLAKE3 checksum makes truncated/garbled paste fail closed in `FromStr`.
- HRP `roomtkt` + separator `1` mirrors the illustrative `roomtkt1q…9z` already shown in
  `docs/getting-started.md`.
- Base32 (RFC 4648 lowercase, no padding) via a small `data-encoding` dependency keeps the
  token compact and copy-paste-safe.

**Zero-new-dependency fallback** (acceptable alternative): encode as
`roomtkt1<hex(version ‖ cbor ‖ checksum)>` using the already-present `hex` crate. Longer
token, no new dependency. See OQ1 — pick one before coding; both round-trip identically.

Rationale: CBOR-over-base32-with-checksum is the iroh-ecosystem idiom for tickets, fails
closed on corruption, and reuses the codec we already trust for byte-exactness. AC4
("capability hash recomputes from ticket secret") is a one-liner:
`ticket.capability_hash() == on_log_event.capability_hash`.

### D5 — Admin authorization gate, checked up front **and** at the event layer

Two layers, both required:

1. **Up-front UX gate (fail fast, write nothing).** Fold the persisted log for `room_id`
   (`RoomMembership::from_events(...).snapshot()`), and require
   `snapshot.admin() == Some(caller_identity)`. A non-admin caller (e.g. a joined member
   who has the genesis in their store) gets an actionable error and the store is untouched.
   This is the primary AC1 mechanism and gives a clean message instead of a cryptic
   rejection.
2. **Event-layer self-check (belt-and-suspenders, before persist).** After building the
   event, re-validate the *whole room log plus the new event* through the fold and assert
   the new event id is `Ingest::Accepted`. This re-proves admin-signer + membership device
   binding + structural validity using the exact code peers will run. If it is not
   accepted, bail **without** persisting (an internal-bug guard, mirroring `room::create`'s
   stateless self-check).

Rationale: layer 1 is the friendly gate; layer 2 guarantees we never persist a
`member.invited` that peers would reject. Together they make AC1 hold both at the CLI and
on the wire.

### D6 — `prev_events` = current room heads, bounded

`prev_events = store.heads(room_id)`. For the single-admin MVP this is typically one head
(the genesis, or the latest admin/membership event). Guard the §6 `MAX_PREV_EVENTS = 20`
bound: if heads somehow exceed 20, cite the 20 **lowest-`event_id`** heads (deterministic);
the uncited heads remain concurrent siblings the sync layer reconciles — DAG correctness is
preserved. In practice this branch never triggers in MVP; log/note it rather than failing.

### D7 — `--expires <DURATION>` parsing → `expires_at`

Accept a compact duration grammar: an integer followed by one of `s` (seconds), `m`
(minutes), `h` (hours), `d` (days) — e.g. `30m`, `24h`, `7d`. Compute
`expires_at = created_at.saturating_add(duration_ms)`. Absent flag ⇒ `expires_at = None`
(no expiry). Reject zero, empty, overflowing, or unsuffixed values with an actionable
error **before** any IO. (Absolute RFC3339 timestamps are deferred — OQ4.)

Note: `created_at` is the advisory wall-clock read (`clock::now_ms`); because expiry is
evaluated log-only against a future `join.created_at`, anchoring `expires_at` to the
admin's `created_at` is exactly the §6 rule.

### D8 — `--role` handling (default `member`)

Default `--role member`. Allow `agent` (PRD §13.3 — agents join by explicit invite).
**Reject `admin`** in the CLI: MVP has a single immutable admin and the fold offers no
second-admin semantics, so issuing an `admin`-role invite is a footgun. (The on-wire enum
still permits `admin`; the CLI is the policy gate.) The chosen role is written verbatim
into `member.invited.role` and must equal the role the joiner later claims (`gate_join`).

### D9 — `--invitee <IDENTITY_ID>` parsing & validation

Parse the 64-char lowercase-hex identity key via `IdentityKey::from_str` (the landed
`public_key_newtype!` `FromStr`). Optionally verify it is a valid Ed25519 curve point
(reuse the key's internal `verifying_key()` path) and reject non-points with a clear
error — an invite to a non-point key can never be joined, so failing early is friendlier.
Reject inviting the **admin's own identity** (self-invite is meaningless) with a clear
message. Warn (do not block) when the invitee is already `Active` in the current snapshot
(re-inviting is legitimate after removal; sticky departure makes a stale invite inert).

### D10 — Output format (script-friendly + AC5)

Print labeled lines to stdout, then the ticket on its own line, then a warning:

```text
invite_id: <hex>
room: blake3:<hex>
invitee: <hex>            # the bound identity key (AC2)
role: member
expires: 2026-07-01T12:00:00Z (in 24h)   # or: expires: never
ticket:
  roomtkt1<...>
warning: this ticket carries a secret — share it over a private channel and treat it like a password.
next: the invitee runs `iroh-rooms room join <ticket>`   # sibling flow
```

Expiry line shows both an absolute time (epoch-ms rendered ISO-8601 UTC) and the
human duration, satisfying "Expiry is encoded and displayed clearly" (AC5). The
secret appears **only** inside the `ticket:` token, never on its own line.

---

## 5. CLI surface (precise)

```text
iroh-rooms [--data-dir <PATH>] room invite <ROOM_ID> --invitee <IDENTITY_ID> [--role <ROLE>] [--expires <DURATION>]
```

- `<ROOM_ID>` — positional, `blake3:<64-hex>` (parsed via `RoomId::from_str`).
- `--invitee <IDENTITY_ID>` — **required**, 64-hex identity key.
- `--role <ROLE>` — optional, `member` (default) | `agent`. `admin` rejected (D8).
- `--expires <DURATION>` — optional, `<int>{s|m|h|d}` (D7).
- Exit `0` + ticket on stdout on success; non-zero + stderr message on any error, with the
  store left unmodified on pre-persist failures.

Wire into `cli.rs`:

```rust
#[derive(Debug, Subcommand)]
enum RoomAction {
    Create { name: String },
    Members { room_id: String },
    /// Mint a key-bound invite ticket for a known invitee identity.
    Invite {
        room_id: String,
        #[arg(long)] invitee: String,
        #[arg(long, default_value = "member")] role: String,
        #[arg(long)] expires: Option<String>,
    },
}
```

The `run` dispatcher parses `room_id`/`invitee`, calls `room::invite(...)`, and prints the
returned `InviteSummary` via a `print_invite` helper (mirror `print_members`).

---

## 6. Module / file plan

| File | Change |
|---|---|
| `crates/iroh-rooms-core/src/event/invite.rs` | **new** — `build_member_invited(...)` + unit/golden tests. |
| `crates/iroh-rooms-core/src/event/mod.rs` | add `pub mod invite;`, `pub use invite::build_member_invited;`, `pub use content::capability_hash;`. |
| `crates/iroh-rooms-core/src/ticket.rs` | **new** — `RoomInviteTicket`, `TicketError`, `Display`/`FromStr`, redacted `Debug`, `capability_hash()`, round-trip + corruption tests. |
| `crates/iroh-rooms-core/src/lib.rs` | add `pub mod ticket;`. |
| `crates/iroh-rooms-cli/src/invite.rs` | **new** — `invite(...) -> Result<InviteSummary>`, duration parsing, `print_invite`, RNG draws, admin gate, persist, ticket build. |
| `crates/iroh-rooms-cli/src/cli.rs` | add `RoomAction::Invite { .. }` + dispatch. |
| `crates/iroh-rooms-cli/src/main.rs` / `lib` wiring | declare `mod invite;` (mirror `mod room;`). |
| `crates/iroh-rooms-cli/Cargo.toml` | add `data-encoding` **iff** base32 encoding chosen (D4); else no change. |
| `crates/iroh-rooms-cli/tests/invite_cli.rs` | **new** — assert_cmd integration suite. |
| `crates/iroh-rooms-core/Cargo.toml` | add `data-encoding` **iff** base32 chosen and ticket codec lives in core. |

No production source is modified by *this planning step*; the table is the build target.

---

## 7. Dependencies to add

- **If base32 encoding is chosen (D4 recommended):** `data-encoding = "2"` in the crate
  that owns the ticket codec (core, per D4). Justify it in the `Cargo.toml` comment style
  the repo uses ("RFC 4648 base32 for the copy-paste invite ticket token"). Note this is a
  *new direct* dependency but **not** a new build artifact: `data-encoding` already resolves
  in the workspace `Cargo.lock` (pulled in transitively by the iroh ecosystem), so the
  dependency cost of the base32 path is essentially zero.
- **Zero new deps otherwise:** the hex fallback uses the already-present `hex` crate; CBOR
  reuses `event::cbor`; RNG reuses `getrandom`; zeroize reuses `zeroize`.

No new runtime/network dependency: `room invite` does **not** depend on `iroh-rooms-net`.

---

## 8. Error model & observability

All errors are `anyhow` with actionable context; nothing secret appears in any message.

| Condition | Behavior |
|---|---|
| Bad `--expires` (empty, `0`, no suffix, overflow) | error **before** IO; store untouched. |
| Bad `--invitee` (not 64-hex / non-curve-point) | error before IO. |
| `--role admin` | error before IO ("admin invites are not supported in MVP"). |
| Self-invite (invitee == admin identity) | error before IO. |
| No local identity | actionable error pointing at `identity create` (reuse `SecretKeys::load`). |
| Unknown `room_id` (no events in store) | error ("no room … run `room create`"). |
| Caller is not the room admin | error ("only the room admin can issue invites"); **AC1**. |
| OS CSPRNG unavailable | error (mirror `room create`'s `getrandom` mapping). |
| Built event fails the fold self-check | internal-error guard; **not persisted**. |
| Store open/write failure | error with the db path; partial state avoided by validate-before-insert. |

Observability: success prints the labeled summary + ticket (the audit record is the
persisted `member.invited` event itself; `room members` will subsequently show the invitee
as `status=invited`, an end-to-end sanity hook). No secret is ever logged.

---

## 9. Security, privacy, reliability

- **AC3 — secret never on the log.** Only `capability_hash` is in event content. A
  regression test re-decodes the persisted event and asserts the raw secret bytes appear in
  **no** field and that the ticket is the sole carrier.
- **Key-binding (AC2).** `invitee_key == --invitee`; the join gate proves
  `sender_id == invite.invitee_key`, so ban-evasion under a fresh key is impossible
  (Spike §6). A test asserts the on-log `invitee_key` equals the requested invitee.
- **Capability soundness (AC4).** `RoomInviteTicket::capability_hash()` recomputes
  `BLAKE3(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)` and must equal the on-log hash.
- **Admin-only (AC1).** Up-front fold gate + event-layer fold self-check (D5); peers
  independently re-enforce via `gate_admin_action`.
- **Expiry (AC5).** Log-only, signed, advisory-clock-free; the CLI only encodes it.
- **Ticket = bearer-of-secret-for-a-named-key.** Anyone who obtains the ticket before
  expiry can attempt to join **as the named key only**. This is the documented MVP
  threat (PRD §13.4 #10): no protection after ticket leak, no native revocation (remove the
  subject instead). The CLI must print the password-grade warning (D10) and the docs must
  say "treat like a password" (already in `getting-started.md`).
- **Secret hygiene.** Secret/secret-bearing buffers in `Zeroizing`; `RoomInviteTicket` has
  a redacted `Debug`; signing secrets never leave `SigningKey`. The ticket token is the one
  intentional place the secret is rendered.
- **Reliability / restart determinism.** The invite is persisted as canonical wire bytes
  into the same append-only `rooms.db`; re-folding reproduces the same `invited` state. No
  derived-state divergence (Spike §9).
- **Privacy.** The `invitee_hint` is optional and non-authoritative; MVP leaves it unset
  unless a future `--hint` flag is added (not in scope). The ticket reveals the room id,
  invitee key, inviter identity, and a discovery hint to whoever holds it — acceptable for
  an out-of-band capability.

---

## 10. Implementation steps (for the executing engineer/agent)

1. **Core builder.** Add `event/invite.rs` with `build_member_invited(...)` (D1). Assemble
   `SignedEvent { schema_version: 1, room_id, sender_id, device_id, event_type:
   MemberInvited, created_at, prev_events, content: Content::MemberInvited(..) }`, then
   `to_csb()` → `sign_csb(device_secret)` → `WireEvent::seal`. Add unit tests: deterministic
   output, decode round-trip of every content field, `expires_at` present/absent,
   `prev_events` preserved, signature verifies under `device_id`. Pin a golden
   `event_id`/`capability_hash` regression vector from **fixed, in-test fixture
   inputs** — mirror `build_room_created`'s golden test, which derives `room_id`
   from seed `0x01;32` + nonce `00010203…0e0f` + `created_at=1_750_000_000_000`
   and asserts it. Choose deterministic fixtures (e.g. the spike's Dave-style
   handles `invite_id=da7e…da7e`, `secret=5ec0da7e…`), compute
   `capability_hash = event::capability_hash(room_id, invite_id, secret)`, and
   assert the value the implementation produces. **Do not** hard-code an external
   "expected hash" constant: `PHASE-0-SPIKE.md` specifies only the derivation
   formula (§7) and example handles, **not** a pinned capability-hash output, so
   the golden vector is an implementation-pinned regression lock, not a
   conformance check against a published value.
2. **Re-exports.** `event/mod.rs`: `pub mod invite; pub use invite::build_member_invited;
   pub use content::capability_hash;` (D3).
3. **Core ticket.** Add `ticket.rs` (D4): the struct, `capability_hash()`, `Display`
   (version ‖ CBOR ‖ checksum → base32/hex with `roomtkt1` HRP), `FromStr` (HRP check →
   decode → checksum verify → CBOR decode → field validation), redacted `Debug`, and
   `TicketError`. Reuse `event::cbor`. Add `pub mod ticket;` to `lib.rs`. Tests:
   round-trip equality, checksum/HRP/version corruption rejection, `capability_hash()`
   matches the same inputs through `event::capability_hash`.
4. **CLI duration parser.** In `invite.rs`, `parse_expires(&str, created_at) ->
   Result<u64>` for `<int>{s|m|h|d}` with overflow/zero/empty/suffix validation (D7) +
   unit tests at boundaries.
5. **CLI orchestration.** `invite(home, room_id, invitee_hex, role, expires_opt)`:
   - validate `role` (D8) and `invitee` (D9) **before IO**;
   - `SecretKeys::load(home)` (admin identity + device);
   - open `rooms.db`; `room_event_ids` empty ⇒ "no room" error;
   - re-validate + fold the log; assert `snapshot.admin() == Some(admin_identity)` else
     "only admin" error (AC1); reject self-invite;
   - `prev_events = store.heads(room_id)` (bound per D6); `created_at = clock::now_ms()`;
   - draw `invite_id` + `Zeroizing` secret via `getrandom::fill`;
   - `capability_hash = event::capability_hash(&room_id, &invite_id, &secret)`;
   - `expires_at = expires_opt.map(|d| parse_expires(d, created_at)).transpose()?`;
   - `wire = build_member_invited(...)`; `validate_wire_bytes(...)` (stateless self-check);
   - fold *log + new event*; assert the new id is `Ingest::Accepted` else internal-error,
     **no persist** (D5 layer 2);
   - `store.insert(&validated)`;
   - build `RoomInviteTicket { discovery: vec![admin_device_id], .. }`; return
     `InviteSummary { invite_id, room_id, invitee_key, role, expires_at, ticket_string }`.
6. **CLI wiring + output.** `cli.rs` `RoomAction::Invite` + dispatch; `print_invite`
   (D10). Declare `mod invite;`.
7. **Cargo.toml.** Add `data-encoding` to the codec-owning crate iff base32 chosen (D4/§7).
8. **Tests.** Core unit/golden (steps 1, 3, 4) + CLI integration (`tests/invite_cli.rs`,
   §11).
9. **Verify.** `scripts/verify.sh` green (fmt, clippy `-D warnings`, all tests).
10. **Docs (flag, do not silently skip).** `docs/getting-started.md` currently shows
    `room invite <ROOM_ID> --expires 24h` **without** `--invitee`. That is now incorrect:
    key-bound invites require `--invitee`. Update the illustrative block (and the ticket
    HRP example if base32 chosen). This doc edit rides the docs-conformance flow; note it in
    the PR (OQ6).

---

## 11. Test strategy

Mapping the issue Test Plan ("admin invite, non-admin rejection, hash verification, expired
invite behavior") to concrete tests:

**Core unit (`event/invite.rs`, `ticket.rs`):**
- `build_member_invited` is deterministic; content round-trips (all fields, `expires_at`
  present/absent, `invitee_hint`); signature verifies under `device_id`; `prev_events`
  preserved; golden `capability_hash`/`event_id` vector.
- Built invite **passes stateless `validate_wire_bytes`**.
- Ticket round-trips `Display`→`FromStr`; corrupted HRP/version/checksum/base32 are
  rejected; `ticket.capability_hash()` equals `event::capability_hash(same inputs)` (AC4).

**Core fold integration (reuse `RoomMembership`):**
- Genesis (admin) + `build_member_invited(admin)` ⇒ invite **Accepted**; invitee folds to
  `status = Invited` (admin invite happy path; AC1 positive).
- Genesis + an invite **signed by a non-admin** key ⇒ `Ingest::Rejected
  (InsufficientRole)` (non-admin rejection; AC1 negative). (If a helper to build a
  non-admin invite is awkward, exercise this through the CLI test below instead.)
- **Expired invite behavior:** an invite with `expires_at = T` and a `member.joined` whose
  `created_at > T` ⇒ join rejected `ExpiredInvite`; with `created_at <= T` ⇒ accepted.
  (Confirm/extend existing `gate_join` expiry coverage in
  `tests/membership_fold.rs`; the new builder makes constructing the invite trivial.)

**CLI Rust API (`invite.rs` `#[cfg(test)]`, no binary spawn):**
- `invite` by the admin returns a summary; the persisted event decodes to
  `Content::MemberInvited` with `invitee_key == requested` (AC2) and the **secret absent**
  from every field (AC3).
- `room::members` after the invite shows the invitee as `status = invited`.
- A **non-admin** home (create identity B, hand B the genesis-only log of A's room, or
  drive via two data-dirs) calling `invite` errors with "only … admin" and **writes
  nothing** (AC1).
- `parse_expires` boundaries: `1s`/`30m`/`24h`/`7d` ok; ``/`0h`/`12`/`5x`/huge → error.
- Self-invite and `--role admin` rejected before IO.

**CLI integration (`tests/invite_cli.rs`, `assert_cmd`):**
- `room invite <room> --invitee <id> --expires 24h` exits 0; stdout has `invite_id:`,
  `invitee:`, `expires:` (absolute + `in 24h`), and a `roomtkt1…` ticket line.
- Output contains **no** secret-seed material (mirror the room-create secret-leak test).
- No-identity / unknown-room / non-admin / bad-duration / bad-invitee each exit non-zero
  with an actionable message and leave `rooms.db` unchanged where pre-persist.
- `--data-dir` isolation honored; `IROH_ROOMS_HOME` cleared in tests.

---

## 12. Risks & mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| Secret accidentally serialized into event content | high | Builder takes only `capability_hash`; explicit AC3 test re-decoding the persisted event; `Content::MemberInvited` has no secret field by construction. |
| Ticket leak grants a join | medium (accepted) | Key-bound (named key only); expiry; password-grade warning; documented MVP limitation (PRD §13.4 #10). No native revocation — remove the subject. |
| Non-admin mints an invite peers reject | medium | Up-front fold admin gate **and** event-layer self-check before persist (D5); peers re-enforce `gate_admin_action`. |
| `prev_events` exceeds `MAX_PREV_EVENTS` | low | Deterministic 20-lowest-id cap (D6); never triggers in single-admin MVP; uncited heads reconcile via sync. |
| Ticket corruption on copy-paste | low | Versioned + BLAKE3-checksummed token; `FromStr` fails closed (`TicketError`). |
| Inviting an already-removed / already-active key | low | Sticky departure makes a stale invite inert; a fresh post-removal invite is the legitimate re-admission path; CLI warns, does not block (D9). |
| Doc/CLI surface mismatch (`--invitee` missing in getting-started) | low | Update the illustrative block (step 10 / OQ6); flagged, not silently skipped. |
| `created_at` clock skew vs expiry | low | Expiry is log-only against `join.created_at`; local clock never authorizes (Spike §6); advisory clock-skew flag is non-rejecting. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Where satisfied | Test |
|---|---|---|
| Only admin can issue an invite | D5 up-front fold gate + event-layer self-check; `gate_admin_action` | non-admin CLI error test; fold reject(InsufficientRole) test |
| Invite bound to invitee identity key | `member.invited.invitee_key == --invitee` (D1/D9) | persisted-event `invitee_key` assertion |
| Capability secret not written to event log | secret only in ticket; content carries `capability_hash` only (D2) | AC3 re-decode-and-search test |
| Capability hash recomputes from ticket secret | `RoomInviteTicket::capability_hash()` (D4) | ticket-vs-log hash equality test |
| Expiry encoded and displayed clearly | `expires_at` in content + D10 output | `parse_expires` tests + CLI stdout `expires:` assertion |

---

## 14. Dependencies & sequencing

- **Hard deps (landed):** #17 (store + `rooms.db` + room create), #12 (fold + admin gate +
  `gate_join`). Both present in `main`.
- **Reuses (landed):** IR-0002 event model, IR-0101 identity/secret loader & clock.
- **Enables (siblings, out of scope):** the `room join` / `member.joined` flow consumes the
  ticket and the on-log invite; the sync/net layers propagate the persisted invite.
- **No dependency on #9 (net):** `room invite` is offline; the discovery hint is the admin
  `device_id` read from the local profile.

---

## 15. Assumptions

1. The node running `room invite` holds the room's admin secrets (single immutable admin =
   creator). A joined member's store contains the genesis, so the admin check is a real
   gate, not a tautology.
2. The `member.invited` content schema, `capability_hash`, and the admin/join fold gates
   are final and **will not change** under this issue — confirmed in
   `event/content.rs` and `membership/fold.rs`.
3. The store at `<HOME>/rooms.db` and `EventStore::heads` are the source of `prev_events`;
   `room_event_ids` + the fold are the source of the admin/membership view.
4. `created_at` (advisory wall-clock) is an acceptable anchor for `expires_at`, since expiry
   is enforced log-only against a future `join.created_at` (Spike §6).
5. The capability secret is exactly 16 bytes (matches `member.joined.capability_secret:
   bstr[16]`); 16 bytes ≥ the Spike's "≥16 bytes" floor.
6. The join flow (ticket consumer) is specified/owned by a sibling issue and will reuse the
   core `RoomInviteTicket` decoder.

---

## 16. Open questions

- **OQ1 — Ticket encoding.** Base32 + `data-encoding` (recommended, compact, matches the
  `roomtkt1…` aesthetic) **vs** hex (zero new dependency). Decide before coding; both
  round-trip identically and use the same CBOR body + checksum.
- **OQ2 — `--role` surface.** Default `member`; allow `agent`; reject `admin` (D8). Confirm
  whether agent invites get dedicated ergonomics now or under the agent-flow issue.
- **OQ3 — Discovery hints richness.** MVP carries the admin `device_id` (`EndpointId`) only,
  relying on iroh n0 discovery. Should the ticket also embed relay URL + direct socket-addr
  hints (a `NodeAddr`) once the join/net flow lands? Recommend deferring to that issue;
  `discovery: Vec<DeviceKey>` leaves room to grow.
- **OQ4 — Duration grammar.** MVP supports `<int>{s|m|h|d}`. Add absolute RFC3339
  `--expires-at` later? Recommend suffix-durations only for MVP.
- **OQ5 — Persist-only vs live push.** `room invite` persists the event and relies on
  sync/net to propagate it (consistent with `room create`). Confirm no synchronous network
  push is expected in MVP.
- **OQ6 — Docs reconcile.** `docs/getting-started.md` Step 3 must add `--invitee
  <INVITEE_ID>` and (if base32 chosen) keep the `roomtkt1…` example. Owned by the
  docs-conformance flow but flagged here so it is not missed.
- **OQ7 — Ticket placement.** Core (recommended, shared with the join flow) vs CLI-only.
  Confirm core so the decoder is not duplicated.
```
