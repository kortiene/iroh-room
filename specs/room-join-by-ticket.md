# Spec: Room Join by Ticket (IR-0104)

- **Issue:** #19 — `[IR-0104] Implement room join by ticket`
- **Parent epic:** #2
- **Labels:** type/feature, type/security, area/protocol, area/cli, priority/p0, risk/high
- **Depends on:** #18 (key-bound invite ticket + `RoomInviteTicket`, **landed**),
  #12 (membership fold + `gate_join`, **landed**), #11 (bounded recent-sync engine, **landed**).
  Transitively reuses #9 (full-mesh QUIC transport / `Node`, **landed**), #20 (online
  `send`/`tail` orchestration pattern, **landed**).
- **Status:** planning — this document is the build plan. No production code is written by
  this step.
- **Traceability:** PRD `PRD.v0.3.md` §15.3, §15.4, §16; `PHASE-0-SPIKE.md` Event
  Protocol §7 (`member.joined`), Membership & Ordering §3.5 / §3.7 / §6, sync §4 / §8
  (membership sub-DAG never windowed), ADR-1 (native admission).

---

## 1. Summary

Add the joiner-side command

```text
iroh-rooms [--data-dir <PATH>] room join <TICKET> [--peer <ENDPOINT_ADDR>]… [--display-name <NAME>] [--timeout <DUR>]
```

which lets an **invited** peer redeem a `roomtkt1…` ticket and become an `Active`
member of the room by authoring a valid `member.joined` event that both peers converge
on. Concretely it:

1. **Decodes** the out-of-band [`RoomInviteTicket`](../crates/iroh-rooms-core/src/ticket.rs)
   (`FromStr`) — fail-closed on a corrupt/garbled paste (AC: deterministic rejection).
2. **Locally pre-checks key binding**: the loaded local identity MUST equal
   `ticket.invitee_key`; a wrong identity is rejected **before any network or store IO**
   (AC2 fast path; the on-log `gate_join` is the convergent authority).
3. **Bootstraps the membership sub-DAG**: brings up an ephemeral [`Node`], dials the
   admin (the ticket's discovery hint / `--peer`), and pulls the never-windowed
   membership + admin chain (genesis + the naming `member.invited` + admin chain) via the
   engine's existing `WantMembership` handshake — "Sync required membership ancestors".
4. **Adds the device binding**: builds a `member.joined` whose `device_binding` attests
   `(sender_id = invitee_key, device_id = local device)` under the validated `room_id`
   (Event Protocol §1).
5. **Proves the capability**: the join carries `via_invite_id` + the ticket's
   `capability_secret`; on **every** peer the landed `gate_join` recomputes
   `BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ via_invite_id ‖ secret)` and matches it against
   the on-log invite's `capability_hash` (AC: bad secret rejected; AC: expiry rejected).
6. **Self-validates + fold-checks** the join locally (stateless §6 pipeline + the fold's
   `gate_join`) and **publishes** it so the admin ingests, validates, folds, and persists
   it — after which the joiner is `Active` on **both** peers (AC5).

**The authorization trust boundary already exists and is conformance-tested.** The fold's
`gate_join` (`crates/iroh-rooms-core/src/membership/fold.rs`) already enforces, against an
event's fixed causal ancestors:

- a naming, **key-bound** admin invite must exist for `sender_id` (wrong identity ⇒
  `BadCapability`);
- the recomputed capability hash must match (`BadCapability` on mismatch);
- log-only, signed-fields **expiry** (`join.created_at > invite.expires_at ⇒
  ExpiredInvite`, never the local clock);
- **role** equality and **sticky departure** (a prior removal/leave consumes the invite).

The `member.joined` **content type, its strict CBOR parser, the device-binding
verification, and the `gate_join` rule are all landed** (see `content.rs` `MemberJoined`,
`verify_bindings`, `fold.rs::gate_join`). The fold already has tests for *valid join →
Active*, *wrong identity → `BadCapability`*, *wrong secret → `BadCapability`*, and *expired
→ `ExpiredInvite`*. **So IR-0104 introduces no new authorization logic.** It is three
bounded additions:

- **(core builder)** a pure `build_member_joined(...)` assembler, the sibling of the landed
  `build_member_invited` / `build_room_created`;
- **(net bootstrap)** a way for a not-yet-`Active` invitee to pull the membership sub-DAG
  from the admin and push its join — the one genuinely new, security-sensitive seam
  (see **§4 D5**, the central design decision);
- **(cli)** a `room join` orchestration that decodes the ticket, drives the bootstrap, and
  builds + publishes the join, mirroring the landed online `send`/`tail` orchestration.

---

## 2. Background & current repository state

### 2.1 What exists (landed work this builds on)

- **`member.joined` content + validation** (`event::content`, IR-0002). The schema is
  final:
  ```rust
  pub struct MemberJoined {
      pub via_invite_id: [u8; 16],          // references member.invited.invite_id
      pub capability_secret: [u8; 16],      // recomputes the invite's capability_hash
      pub role: String,                     // MUST equal the invite's role (gate_join)
      pub device_binding: DeviceBinding,    // self-contained; verified statelessly
      pub display_name: Option<String>,
  }
  ```
  `member.joined` is a **self-contained-binding** type
  (`requires_membership_device_binding() == false`): its device binding is verified by the
  stateless layer (`verify_bindings`), not resolved from prior membership state — exactly
  right for a first-time joiner whose device the room has never seen.
- **`gate_join`** (`membership/fold.rs`): the full key-bound join gate (admin present,
  naming key-bound capability-matching invite, expiry, role, sticky departure). Returns
  `BadCapability` / `ExpiredInvite` / `InsufficientRole` deterministically from the event's
  ancestor view. **This is the convergent authority** — every peer re-runs it on the same
  set and reaches the same verdict (§0 same-set convergence).
- **`capability_hash`** and **`DeviceBinding::create(room_id, identity_secret, device_key)`**
  — the exact §7 / §1 derivations the builder needs.
- **`RoomInviteTicket`** (`core::ticket`, IR-0103): `FromStr`/`Display`, fail-closed
  decoding (`TicketError::{BadPrefix,BadBase32,Truncated,UnsupportedVersion,BadChecksum,
  MalformedBody}`), `capability_hash()`, redacted `Debug`. Carries `room_id`, `invite_id`,
  `capability_secret`, `invitee_key`, `role`, `expires_at`, `inviter_identity`, and
  `discovery: Vec<DeviceKey>` (MVP = the admin's `device_id`/`EndpointId`).
- **SQLite event store** (`store`, IR-0004): `open` / `insert` / `get` / `room_event_ids` /
  `heads` / `by_type`. A joiner starts with **no rows for this room** — the store is created
  on first open and filled by the bootstrap.
- **Bounded recent-sync engine** (`sync`, IR-0007): `SyncEngine::{open, on_connect,
  on_message, publish, snapshot, heads, room_tail, completeness, ...}`. Critically,
  `on_connect` **already** emits `WantMembership { have }`, and `serve_want_membership`
  responds with the **never-windowed** membership sub-DAG + full admin chain (sync §4 / §8
  hard invariant). This is the exact mechanism for "sync required membership ancestors" —
  no new sync code is needed for the *pull*.
- **Full-mesh QUIC carrier + `Node`** (`net`, IR-0005): `Node::{spawn, connect_to, publish,
  store_contains, snapshot, room_tail, peer_state, endpoint_addr, shutdown}`. The
  `EventProtocolHandler` enforces **admission before bytes**:
  `AllowlistAdmission` resolves `device_id → identity → Active?` and **closes the connection
  before `accept_bi()`** for any non-Active / unbound device (`handler.rs`,
  `admission.rs`). This is a landed, tested security guarantee — and the crux of the
  bootstrap problem (§2.2 / §4 D5).
- **Online orchestration pattern** (`cli::message`, IR-0105): `fold_room`, `select_heads`,
  `build_admission`, `build_dial_set`, `endpoint_id_of`, `net_mode`, `parse_peers`,
  `parse_peer`, `render_endpoint_addr`, `parse_timeout`, and the `--loopback`(hidden) /
  `--peer` conventions, plus the scoped Tokio `runtime()` helper (which lives in `cli.rs`,
  the dispatcher, **not** in `message.rs`). `room join` is the third online command and
  reuses all of these. *(Visibility note for the executor: most `message.rs` helpers are
  `pub(crate)`, but `parse_peer` is currently a private `fn` and `parse_timeout` is `pub` —
  promote `parse_peer` to `pub(crate)` when `join.rs` needs it; this is the only promotion
  required.)*

### 2.2 The real gap (this issue closes it)

There is **no way to author a `member.joined` event** (`grep "build_member_" event/` shows
`build_room_created`, `build_member_invited`, `build_message_text`, `build_pipe_*` — **no
`build_member_joined`**). `room create` mints the genesis, `room invite` mints the invite;
**nothing mints the join**. The CLI `room` group has `create`, `members`, `invite`, `send`,
`tail` but **no `join`**. `docs/getting-started.md` Step 3 already advertises the UX
(`iroh-rooms room join <BOB_TICKET>` → `Joined room "…" as member.`) but it is scaffold.

The two-peer message e2e test (`net/tests/message_e2e.rs`) deliberately **pre-seeds** Bob's
membership "via core event builders to avoid the `room join` CLI dependency (#19 / OQ-5)" —
i.e. the bootstrap that #19 must build is exactly what the existing tests stub out.

**The new, hard part is the network bootstrap of a not-yet-`Active` invitee** (§4 D5):
the admin's admission gate closes the connection *before bytes* for any device that is not
bound to an `Active` identity. A first-time joiner is at most `Invited`, and its **device is
unknown to the admin** until the join itself arrives. So the joiner cannot, under the
landed admission gate unmodified, either (a) pull the membership sub-DAG or (b) push its
join. Resolving this — without weakening the `gate_join` authorization authority — is the
central design decision of this issue and the reason for its `risk/high` label.

### 2.3 Spike / PRD facts that constrain the design

- **Key-bound only.** A join is valid iff it cites a naming `member.invited` with
  `invitee_key == sender_id` and proves the capability secret (Spike §6 path-A, §3.5).
  Ban-evasion under a fresh key is structurally impossible — there is no naming invite for
  it. The CLI's local pre-check (identity == `ticket.invitee_key`) is a **friendly fast
  fail**, not the security boundary; `gate_join` is.
- **The capability secret travels in the join content.** Unlike `member.invited` (hash
  only), `member.joined.capability_secret` is **on the log** (Spike §7 schema). After a
  join the secret is public — acceptable because the invite is key-bound and consumed by
  departure; a replay under another key fails the key-binding gate. (Implication: the
  bootstrap need not invent a separate "prove the secret" channel — the join *is* the
  proof, and the secret is already destined for the log.)
- **Expiry is log-only and clock-free.** `join.created_at` is the joiner's signed,
  advisory wall-clock read; expiry is `invite.expires_at` absent OR
  `join.created_at <= invite.expires_at`. The local clock is **never** an authorization
  input; the clock-skew check is strictly advisory (Spike §6 "Expiry determinism", §20).
- **`prev_events` = room heads, and must descend from the invite.** `member.joined` cites
  the current room DAG heads (Spike §7 table: "room heads (must include / descend from the
  referenced invite)"). After the membership pull the joiner knows those heads; citing them
  places the invite in the join's causal ancestors, which `gate_join` requires.
- **Membership sub-DAG is never windowed.** The complete authorization chain (genesis + all
  membership events for relevant subjects + the full admin chain), **causally closed** over
  `prev_events` ancestry, MUST always sync (sync §4 / §8): an invite minted after a
  conversation cites chat heads as structural parents, and the joiner's fold cannot classify
  it without them — the closure is what keeps a post-conversation join bootstrappable while
  the provisional filter denies by-id backfill. The engine's `WantMembership` implements this.
  Known trade-off: while `--accept-joins` holds the bootstrap window open, a provisionally
  admitted dialer (which has not yet proven invite possession) can pull the closure — including
  any chat that entered the membership ancestry — just as it could already pull every
  admin-authored event. Scoping the bootstrap serve to capability provers is future hardening.
- **Single immutable admin.** The room has exactly one admin (the creator). The ticket's
  `inviter_identity` is that admin; `discovery[0]` is the admin's `device_id`. The admin is
  the natural bootstrap peer (it authored the invite, so it provably holds the
  membership sub-DAG).
- **Native admission is load-bearing (ADR-1).** "Admission is a property of the transport;
  reject a non-member at connect time, before any event byte flows." Any bootstrap design
  must preserve the *authorization* guarantee and minimize any *privacy* regression
  (§4 D5, §9).

### 2.4 Workspace conventions to honor

- **Pure, deterministic core assemblers**: the builder takes injected `created_at` /
  `prev_events` and contains no wall-clock or RNG (mirror `build_member_invited`).
- **Validate-before-publish / persist**: self-validate the freshly built join through the
  real §6 pipeline *and* the fold before it leaves the process (mirror `room::send`).
- **Offline-guarantee vs online-best-effort** does **not** apply: unlike `send`, a join is
  inherently online (it must reach the admin to bootstrap and to be observed on both
  peers). A join that never reaches a bootstrap peer is a **failure**, not a silent
  local-only success (§8).
- **Secret hygiene**: the `capability_secret` lives in a `Zeroizing` buffer from
  ticket-decode until it is placed in the join content; `RoomInviteTicket`'s `Debug` is
  already redacted; no secret on any error/log path. (Note the secret legitimately lands on
  the log inside the join — that is the protocol, not a leak.)
- **Pre-IO validation**: ticket decode, identity match, and option parsing run before any
  network/store IO; a bad invocation writes nothing and dials nothing.
- **Errors → stderr + non-zero exit; success → stdout + exit 0.**
- **`scripts/verify.sh`** (fmt + clippy `-D warnings` + workspace tests) is the gate.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. A pure core builder `build_member_joined(...)` that assembles + signs a `member.joined`
   from injected `via_invite_id`, `capability_secret`, `role`, `device_binding` inputs,
   `display_name`, `prev_events`, and `created_at`.
2. The `room join <TICKET>` CLI orchestration: decode ticket → local key-binding pre-check →
   bring up node → dial admin → pull membership sub-DAG → build + self-validate + fold-check
   the join → publish → confirm the joiner is `Active` locally and reachable on the admin.
3. The **network bootstrap seam** that lets a not-yet-`Active` invitee pull the membership
   sub-DAG and push its join past the admin's admission gate **without weakening
   `gate_join`** (§4 D5). This includes the admin-side "joinable" affordance (the admin must
   be online and accepting joins).
4. `--peer <ENDPOINT_ADDR>` (repeatable, deterministic LAN/CI dial), `--display-name`
   (optional `member.joined.display_name`), `--timeout`, and the hidden `--loopback` flag
   for deterministic tests — consistent with `send`/`tail`.
5. Script-friendly output: the room id/name, the joined role, the join `event_id`, and a
   next-step hint; an actionable error on every rejection class.
6. Tests: core builder unit/golden; CLI arg/decode/pre-check unit tests; and a **two-peer
   integration test** (loopback) covering valid join, wrong identity, expired invite, and
   bad secret, asserting the joiner appears in `room members` on **both** peers (AC5).

### 3.2 Out of scope (sibling issues / already landed)

- **The `gate_join` authorization rule itself** — landed and conformance-tested (#12). This
  issue *drives* it, it does not reimplement it.
- **`member.left` / `member.removed` CLI** (leave/kick) — separate issues under #2.
- **Open/bearer tickets, `max_uses`, invite revocation, key rotation** — excluded from MVP
  (Spike §6; PRD §13.4/§13.5).
- **Multi-admin or admin-offline join** — MVP joins bootstrap from the single admin. Joining
  from a non-admin active member who happens to hold the sub-DAG is a possible generalization
  (OQ4) but not required.
- **Full real-NAT confirmation (Gate A)** — inherited open risk from #9; the loopback
  integration test is **not** Gate A (`net/NOTES.md`).
- **Self-contained tickets that embed the genesis/invite wire bytes** (a join-without-pull
  alternative) — considered and deferred (§4 D5 Alt-C / OQ2); would change the landed #18
  ticket format.

### 3.3 Why the split is safe

Authorization is enforced **on every peer** by the landed stateless validator + `gate_join`
against each event's fixed ancestors. A buggy or hostile bootstrap **cannot** manufacture
membership: a join is `Active`-making only if it cites a real naming invite, proves the
capability, is unexpired, role-matches, and is not consumed by a prior departure. The
bootstrap seam (§4 D5) is therefore a **liveness + privacy** mechanism (how the bytes get
there), never an **authorization** mechanism (whether they count). This is the same
"transport carries, log decides" split the rest of the system relies on.

---

## 4. Key design decisions

### D1 — Pure `build_member_joined` builder in core (recommended)

Add `crates/iroh-rooms-core/src/event/join.rs` exporting `build_member_joined`, re-exported
as `event::build_member_joined` (sibling of `build_member_invited`).

```rust
/// Assemble and sign a joiner's `member.joined` event (Event Protocol §7).
///
/// Pure and deterministic: with the same inputs it yields byte-identical output.
/// `created_at` and `prev_events` are injected by the caller so this stays free of
/// wall-clock and RNG. The event is signed by the joiner's **device** secret; the
/// signature MUST verify under `device_id`. The self-contained `device_binding` is
/// supplied by the caller (built via `DeviceBinding::create(room_id, identity_secret,
/// device_key)`); the stateless layer (`verify_bindings`) checks it attests exactly
/// `(sender_id, device_id)` under `room_id`.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_member_joined(
    invitee_identity_secret: &SigningKey, // provides sender_id (== ticket.invitee_key)
    invitee_device_secret: &SigningKey,   // signs the event; device_id
    room_id: &RoomId,
    via_invite_id: &[u8; SHORT_ID_LEN],
    capability_secret: &[u8; SHORT_ID_LEN],
    role: &str,
    device_binding: DeviceBinding,
    display_name: Option<&str>,
    prev_events: &[EventId],
    created_at: u64,
) -> WireEvent
```

Rationale: matches the landed builder pattern exactly (one golden test, reusable by the CLI
and by tests/sims that today hand-assemble `MemberJoined`). Accepting a pre-built
`DeviceBinding` (rather than the device secret alone) keeps the builder's single
responsibility "assemble + sign content" and mirrors how `room.created`'s binding is built
by the caller. *(Acceptable variant: take `invitee_device.device_key()` and build the
binding internally — but passing the binding keeps the assembler symmetric with the genesis
flow and easier to golden-test in isolation.)*

Re-export in `event/mod.rs`: `pub mod join; pub use join::build_member_joined;`.

### D2 — `capability_secret` provenance and hygiene

The secret comes from `RoomInviteTicket::capability_secret` (decoded from the token). Hold
it in a `Zeroizing<[u8; 16]>` from decode until it is copied into the join content. It is
**not** drawn from RNG here (unlike the invite's secret); core stays RNG-free. The secret
will be written to the log inside the join (by protocol) — this is expected, not a leak; the
CLI still never prints it to stdout/stderr.

### D3 — Decode + local key-binding pre-check, before any IO

```rust
let ticket: RoomInviteTicket = ticket_str.trim().parse()      // fail-closed (TicketError)
    .context("could not decode invite ticket")?;
let secret = identity::SecretKeys::load(home)?;               // local identity + device
if secret.identity.identity_key() != ticket.invitee_key {
    bail!("this ticket is bound to a different identity ({}); your identity is {}. \
           Ask the admin to invite {} instead.",
          ticket.invitee_key, secret.identity.identity_key(), secret.identity.identity_key());
}
```

This makes the **wrong-identity** acceptance criterion a friendly, fast, no-network failure
(AC: "Wrong identity key cannot use the ticket") while the on-log `gate_join` remains the
convergent authority for any peer that did not run this pre-check. Also validate `--timeout`
and `--peer` before IO. **Do not** locally enforce expiry here as a hard reject (it is
log-only and clock-free); optionally print an advisory note if the ticket *appears* expired
against the local clock, but still attempt the join so the deterministic on-log verdict
(`ExpiredInvite`) is what the user sees — see D8.

### D4 — Resolve the bootstrap dial set from the ticket + `--peer`

The dial target is the admin: `ticket.discovery` (MVP = `[admin device_id]`) gives the
`EndpointId`; `--peer <ENDPOINT_ID>[@ip:port,…]` supplies explicit socket addresses for
deterministic LAN/loopback dials (real-network mode resolves the bare `EndpointId` via n0
discovery). Build `EndpointAddr`s by pairing each `ticket.discovery` id with a matching
`--peer` address when present, else a bare `EndpointAddr::new(id)` (mirror
`message::build_dial_set` id-matching). At least one reachable bootstrap peer is required;
zero ⇒ actionable error ("could not reach the room admin to bootstrap the join; pass
`--peer <admin-addr>` or check connectivity").

### D5 — The bootstrap seam: how a not-yet-`Active` invitee pulls the sub-DAG and pushes the join *(CENTRAL DECISION — needs architect/security sign-off, see OQ1)*

**Problem.** The admin's `EventProtocolHandler` calls `admission.authorize(remote_device)`
and **closes before `accept_bi()`** unless the device is bound to an `Active` identity
(`handler.rs`). A first-time joiner: (a) is at most `Invited`, never `Active`; (b) has a
device the admin has *never seen* (the binding is *inside* the join that hasn't arrived). So
device-keyed admission **cannot** recognize the joiner. Without a change, the joiner cannot
pull the sub-DAG (step 3) or push the join (step 6).

Three approaches were considered. The recommendation is **Approach A for the MVP slice**,
with **Approach B documented as the security-hardening fast-follow**.

#### Approach A — Provisional bootstrap admission overlay (RECOMMENDED for MVP)

Add a third admission outcome and a bootstrap-scoped accept path:

- **`AdmissionDecision::AdmitProvisional`** (new variant in `admission.rs`). An admin node
  that is *hosting joins* returns this for a device that is **not** an `Active` member **iff
  the room currently has at least one live, unconsumed `member.invited`** (i.e. someone
  could legitimately be joining). With no open invites, the node behaves exactly as today
  (reject before bytes) — so a quiescent room admits no strangers.
- **Bootstrap-scoped accept** (`handler.rs`). On `AdmitProvisional` the handler accepts the
  bidi stream but marks the connection **provisional**: the engine serves it **only** the
  membership sub-DAG (`WantMembership`/`WantEvents` restricted to membership+admin events)
  and accepts **only** a single `member.joined` frame from it. Chat/file/pipe pulls and any
  non-join event from a provisional peer are dropped. A provisional connection that does not
  yield an accepted join within a bounded window (e.g. `2 × timeout`) is closed.
- **Upgrade-on-learn.** When the provisional peer's pushed `member.joined` is **accepted by
  the fold** (`Ingest::Accepted`), the engine has learned the device binding and the subject
  is now `Active`; the admin re-points admission for that device to full membership (the same
  "tear-down/​upgrade-on-learn" shape the pipe watcher already uses). Subsequent traffic is
  normal member traffic.
- **Admin "joinable" affordance.** The admin must be running an online session that uses the
  provisional-aware admission gate. Recommended: a **`room host <ROOM_ID>`** command (a
  long-running admin session: brings up a `Node`, prints `listening:`, serves joins + tails)
  — *or*, to minimize new surface, teach the existing `room tail` to build a
  provisional-aware admission gate when the caller is the admin and live invites exist
  (`--accept-joins`, default on for the admin). **Pick one in OQ3.** Either way, the joiner's
  bootstrap peer is an online admin.

Security/privacy analysis (the reason for sign-off):

- **Authorization is unchanged.** `gate_join` still decides membership on every peer; a
  provisional peer that fails the capability/key/expiry/role/sticky checks produces a
  `Rejected` join that grants nothing anywhere. Approach A cannot create a member.
- **The regression is *privacy*, bounded.** A dialer who knows `room_id` **and** the admin's
  `EndpointId` **and** dials **during an open-invite window** can pull the membership
  sub-DAG — which contains signed, **secret-free** events (genesis, invites carry only
  `capability_hash`, joins/removals). It does **not** leak any capability secret or admit
  the dialer. Mitigations: provisional admission only while invites are open; serve **only**
  the membership sub-DAG to provisional peers (never chat/files/pipes); bounded provisional
  window; stable audit lines (`join.bootstrap.admitted` / `join.bootstrap.rejected:<cause>` /
  `join.accepted`). This is a deliberate, documented relaxation of the "no bytes to
  non-members" perimeter, scoped to the join handshake. **Flag for the security reviewer.**

#### Approach B — Dedicated capability-proving join ALPN (`/iroh-rooms/join/1`) (RECOMMENDED hardening / alternative)

A separate accept chain where the joiner first sends a `JoinHello` proving possession of the
named capability (e.g. the `invite_id` + a signature by the invitee identity over a
server-issued challenge, or directly the `member.joined` it intends to commit). The handler
validates the proof against the on-log invite (`gate_join` shape) **before** serving any
sub-DAG, preserving the "non-member gets nothing" invariant — a stranger without the
capability gets zero bytes. Cost: a new ALPN, handler, message types, and tests — materially
more net protocol than Approach A. Best as a fast-follow once the join flow is proven, or as
the chosen approach if the architect prioritizes the privacy invariant over slice size.
*(Note: because `member.joined` already carries the secret to the log, "prove the
capability" reduces to "send the join first and let `gate_join` judge it"; the only real
addition over Approach A is gating the **sub-DAG service** on that proof.)*

#### Approach C — Self-contained ticket (join without a pull) (deferred)

Extend the ticket to carry the genesis + naming invite wire bytes (or at least the invite
`event_id`), so the joiner can assemble the join **offline** and the bootstrap collapses to a
single push (admin validates via `gate_join`, then sync backfills the rest to the joiner).
Attractive (no pull-before-build, simplest joiner), but **changes the landed #18 ticket
format** and bloats the token. Deferred; tracked as OQ2.

**Recommendation:** implement **Approach A** for IR-0104 (smallest net change, reuses the
landed `WantMembership` pull and `gate_join`, fully testable on loopback), explicitly
flag the privacy trade-off, and file **Approach B** as the security-hardening follow-up.

### D6 — Joiner orchestration sequence (single bootstrap connection)

```text
1. decode ticket; load secrets; assert identity == ticket.invitee_key (D3).
2. open <HOME>/rooms.db (created empty on first join); SyncEngine::open(room_id).
3. build the joiner admission gate from the ticket: bind admin device → inviter_identity,
   set inviter_identity Active (so the admin may dial back / post-join messaging works).
4. Node::spawn; print listening: addr; node.connect_to(admin addr(s)) (D4).
5. wait (≤ timeout) until the local store contains the genesis AND the naming invite —
   i.e. snapshot.status(self) == Some(Invited) (the WantMembership pull populated it).
   On timeout: actionable error (could not bootstrap membership; admin offline?).
6. heads = node.heads().await (now descend from the invite); created_at = clock::now_ms().
7. binding = DeviceBinding::create(room_id, &secret.identity, secret.device.device_key()).
8. wire = build_member_joined(.., via_invite_id = ticket.invite_id,
        capability_secret = ticket.capability_secret, role = ticket.role,
        binding, display_name, &heads, created_at).
9. validate_wire_bytes(&wire) (stateless self-check) → internal-error guard.
10. local fold-check: RoomMembership over (pulled events + new join); assert
    Ingest::Accepted. A local Rejected here maps the protocol reason to a user message:
        BadCapability  -> "this ticket's secret/identity does not match the invite"
        ExpiredInvite  -> "this invite has expired"
        InsufficientRole -> "the ticket's role does not match the invite"
    (These are the deterministic verdicts; failing locally avoids a doomed push.)
11. node.publish(wire_bytes)  -> admin ingests/validates/folds/persists the join.
12. wait (≤ timeout) until snapshot.status(self) == Some(Active) locally; best-effort
    confirm the admin observed it (e.g. it remains connected / heads advanced).
13. shutdown; print the JoinSummary; persist is guaranteed locally (publish path stores;
    a final idempotent insert covers the no-peer edge, mirroring room::send).
```

Step 10's **local fold-check is also where wrong-secret / expired produce a clean local
error** even before the network round-trip — but the *authoritative* rejection still happens
on the admin and on any peer (so the integration test asserts both the CLI error *and*
non-membership on the admin).

### D7 — `prev_events` = pulled room heads, bounded

After the pull, `prev_events = node.heads()` (the membership/admin heads the joiner now
holds), truncated deterministically to `MAX_PREV_EVENTS` (the 20 lowest-id heads), reusing
the exact `select_heads` logic landed in `message.rs`/`invite.rs`. The cited heads descend
from (or include) the invite, satisfying the §7 "must include / descend from the referenced
invite" rule and placing the invite in the join's ancestors for `gate_join`.

### D8 — Expiry handling: log-only, deterministic (AC: "Expired invite is rejected deterministically")

The CLI never uses the local clock to authorize. Expiry is decided by `gate_join`
(`join.created_at > invite.expires_at ⇒ ExpiredInvite`) in step 10 (local) and on the admin
(authoritative). `join.created_at = clock::now_ms()` is the joiner's signed read. The CLI may
print an **advisory** "note: this ticket's expiry has passed" if `now_ms > ticket.expires_at`
before attempting, but the *reject* is the deterministic on-log verdict, identical on every
peer. This is the §6 "Expiry determinism" rule; the integration test sets the invite's
`expires_at` in the past relative to the (advisory) `created_at` and asserts `ExpiredInvite`
on both peers regardless of wall-clock.

### D9 — Role from the ticket; `member` | `agent`

`member.joined.role = ticket.role`. `gate_join` requires it to equal the invite's role. The
ticket's role was validated at invite time (`admin` rejected by #18's CLI), so the joiner
copies it verbatim. No `--role` flag on join (the role is fixed by the invite).

### D10 — Output (script-friendly)

```text
joined: <join event_id>
room: blake3:<hex>        # the room id
name: "<room name>"        # resolved from the pulled genesis, when available
role: member               # the joined role
members: <N> active        # post-join snapshot count, sanity hook
next: run `iroh-rooms room members <room_id>` or `iroh-rooms room tail <room_id>`
```

On the unhappy path, exit non-zero with one actionable line (the mapped reject reason, the
bootstrap-timeout message, the wrong-identity message, or the ticket-decode error). The
`capability_secret` never appears in any output.

---

## 5. CLI surface (precise)

```text
iroh-rooms [--data-dir <PATH>] room join <TICKET> [--peer <ENDPOINT_ADDR>]… [--display-name <NAME>] [--timeout <DUR>] [--loopback]
```

- `<TICKET>` — positional, the `roomtkt1…` token from `room invite` (parsed via
  `RoomInviteTicket::from_str`).
- `--peer <ENDPOINT_ADDR>` — repeatable, `<ENDPOINT_ID>[@<ip:port>[,<ip:port>…]]` (reuse
  `message::parse_peer`); supplies socket addresses for deterministic LAN/CI dials. The
  bootstrap target id comes from `ticket.discovery`; `--peer` adds addressing.
- `--display-name <NAME>` — optional `member.joined.display_name` (bounded like other
  free-text content; reuse the §7 content bound). Absent ⇒ `None`.
- `--timeout <DUR>` — `<int>{ms|s|m}` (reuse `message::parse_timeout`); bounds both the
  membership-pull wait and the post-publish confirmation wait. Default e.g. `10s`.
- `--loopback` — hidden; deterministic CI/LAN stack (reuse `message::net_mode`).
- Exit `0` + `JoinSummary` on stdout on success; non-zero + one stderr line on any failure.

Wire into `cli.rs` `RoomAction`:

```rust
/// Redeem an invite ticket and join the room as an active member.
Join {
    /// The roomtkt1… ticket printed by `room invite`.
    ticket: String,
    #[arg(long = "peer")] peers: Vec<String>,
    #[arg(long = "display-name")] display_name: Option<String>,
    #[arg(long, default_value = crate::join::DEFAULT_JOIN_TIMEOUT)] timeout: String,
    #[arg(long, hide = true)] loopback: bool,
},
```

Dispatch parses `--timeout` before IO, then `runtime()?.block_on(join::join(&home, &ticket,
&peers, display_name.as_deref(), timeout, loopback))` and prints via `join::print_join`
(mirror the `Send` arm). If the admin-side affordance is a new command (D5/OQ3), also add a
`Host { room_id, peers, loopback }` arm.

---

## 6. Module / file plan

| File | Change |
|---|---|
| `crates/iroh-rooms-core/src/event/join.rs` | **new** — `build_member_joined(...)` + unit/golden tests. |
| `crates/iroh-rooms-core/src/event/mod.rs` | add `pub mod join;` + `pub use join::build_member_joined;`. |
| `crates/iroh-rooms-cli/src/join.rs` | **new** — `join(...) -> Result<JoinSummary>`, ticket decode, key-binding pre-check, bootstrap orchestration, `print_join`; `DEFAULT_JOIN_TIMEOUT`. Reuses `message::{fold_room, select_heads, build_admission, build_dial_set, endpoint_id_of, net_mode, parse_peers, parse_timeout, render_endpoint_addr}` (promote any needed `pub(crate)` helpers, already mostly `pub(crate)`). |
| `crates/iroh-rooms-cli/src/cli.rs` | add `RoomAction::Join { .. }` (+ optional `Host`) + dispatch. |
| `crates/iroh-rooms-cli/src/main.rs` | declare `mod join;` (mirror `mod message;`). |
| `crates/iroh-rooms-net/src/admission.rs` | **(Approach A)** add `AdmissionDecision::AdmitProvisional`; a provisional-aware admission impl (e.g. `bind`/`set_active` plus a `joinable`/open-invite flag), or a wrapper that consults live invites. |
| `crates/iroh-rooms-net/src/handler.rs` | **(Approach A)** handle `AdmitProvisional`: bootstrap-scoped accept (membership-only service + single-join acceptance + bounded window); upgrade-on-learn. |
| `crates/iroh-rooms-net/src/node.rs` / `transport.rs` | **(Approach A)** thread the provisional path; expose a way to update admission on join-accept (upgrade-on-learn), mirroring the pipe watcher's live re-evaluation. |
| `crates/iroh-rooms-net/src/audit.rs` | **(Approach A)** add stable `join.bootstrap.*` / `join.accepted` audit vocabulary. |
| `crates/iroh-rooms-cli/tests/join_cli.rs` | **new** — assert_cmd arg/decode/pre-check tests (no network). |
| `crates/iroh-rooms-net/tests/join_e2e.rs` | **new** — two-peer loopback integration: valid / wrong-identity / expired / bad-secret + both-peers-`room members` after sync (AC5). |
| `docs/getting-started.md` | reconcile Step 3 join half + the admin "joinable" step (rides the docs-conformance flow; flag in PR — OQ6). |

No production source is modified by *this planning step*; the table is the build target.

> **Scope note.** The core builder + CLI orchestration are small and low-risk. The
> **net-layer bootstrap (Approach A)** is the bulk of the effort and the security-sensitive
> part; if the architect prefers Approach B or wants the privacy invariant preserved, the
> net rows change shape (new ALPN/handler) — settle OQ1 before building.

---

## 7. Dependencies to add

- **No new external crates.** Ticket decode reuses landed `core::ticket` + `data-encoding`
  (already present); the builder reuses `signed`/`wire`/`cbor`; the bootstrap reuses the
  landed `SyncEngine`/`Node`/`tokio`. The provisional-admission change is internal to the
  `net` crate.
- `room join` depends on `iroh-rooms-net` (it is an **online** command), unlike the offline
  `room invite`.

---

## 8. Error model & observability

All errors are `anyhow` with actionable context; no secret appears in any message.

| Condition | Behavior |
|---|---|
| Malformed/corrupt ticket | error before IO (`TicketError` → context); nothing dialed. |
| Local identity ≠ `ticket.invitee_key` | error before IO (AC: wrong identity, fast path). |
| No local identity | actionable error → `identity create`. |
| Bad `--timeout` / `--peer` | error before IO. |
| No reachable bootstrap peer / membership pull times out | error ("could not bootstrap membership from the admin; is the admin online? pass `--peer`"). **A join is online; this is a failure, not a silent local success.** |
| Wrong capability secret | local fold-check → `BadCapability` → "the ticket secret/identity does not match the invite"; also rejected on the admin (authoritative). |
| Expired invite | local + admin `gate_join` → `ExpiredInvite` → "this invite has expired"; deterministic on both peers (D8). |
| Role mismatch | `InsufficientRole` → "the ticket role does not match the invite". |
| Built join fails stateless self-validation | internal-error guard; not published. |
| Store / node bring-up failure | error with path/context. |

Observability: success prints `JoinSummary`; the **persisted `member.joined` is the audit
record**, and `room members` on both peers is the end-to-end oracle. The admin's node emits
stable `join.bootstrap.admitted` / `join.bootstrap.rejected:<cause>` / `join.accepted` audit
lines (Approach A). No `capability_secret` is ever logged by the CLI.

---

## 9. Security, privacy, reliability

- **AC: only a valid invited identity can join.** `gate_join` requires a naming, key-bound,
  capability-matching, unexpired, un-consumed admin invite; enforced on every peer. The
  CLI's identity pre-check is a friendly fast-fail, not the boundary.
- **AC: wrong identity key cannot use the ticket.** The invite is key-bound; a join under a
  different `sender_id` finds no naming invite ⇒ `BadCapability` (fold test
  `join_by_wrong_identity_rejected`). The CLI pre-check rejects it before any IO.
- **AC: bad capability secret is rejected.** `gate_join` recomputes the hash from the
  supplied secret; mismatch ⇒ `BadCapability` (fold test
  `join_with_wrong_capability_secret_rejected`).
- **AC: expired invite is rejected deterministically.** Log-only, signed-fields comparison;
  no local clock (fold test `join_after_invite_expiry_rejected`; D8).
- **Capability secret is public after the join — by design.** `member.joined.capability_secret`
  is on the log (Spike §7). Key-binding + departure-consumption make a replay under another
  key inert. The CLI keeps the secret in `Zeroizing` until it is placed in the join and never
  prints it; landing it on the log is the protocol, not a leak.
- **Bootstrap privacy trade-off (Approach A).** Provisional admission discloses the
  **secret-free** membership sub-DAG to a dialer who knows `room_id` + admin `EndpointId` and
  dials during an open-invite window. It never admits the dialer and never leaks a secret.
  Mitigations: open-invites-only, membership-only service, bounded window, audit lines.
  **This is the headline security decision (OQ1) and must be reviewed.** Approach B removes
  the trade-off at the cost of a new ALPN/handshake.
- **Admission invariant preserved for non-bootstrap traffic.** Chat/file/pipe planes are
  unchanged: a provisional peer is served only the membership sub-DAG and accepted for only a
  single join; everything else still fails closed.
- **Reliability / restart determinism.** The join persists as canonical wire bytes into the
  joiner's `rooms.db`; re-folding reproduces `Active`. The same bytes on the admin fold to
  the same membership (§0 same-set convergence). A join that reaches no bootstrap peer is a
  clean failure (no half-membership): nothing is `Active` until the admin accepts the join.
- **Concurrency.** A concurrent kick of the joiner converges to `Removed` (Removed-dominates);
  re-admission needs a fresh post-departure invite (Spike §3.7). The CLI need not special-case
  this — the fold does.

---

## 10. Implementation steps (for the executing engineer/agent)

1. **Core builder.** Add `event/join.rs::build_member_joined` (D1). Assemble
   `SignedEvent { schema_version, room_id, sender_id = invitee_identity, device_id =
   invitee_device, event_type: MemberJoined, created_at, prev_events, content:
   Content::MemberJoined(..) }`, then `to_csb` → `sign_csb(device_secret)` → `WireEvent::seal`.
   Re-export in `event/mod.rs`. Tests: deterministic output; content round-trips (all fields,
   `display_name` present/absent); signature verifies under `device_id`; `prev_events`
   preserved; the built join **passes stateless `validate_wire_bytes`**; an
   **implementation-pinned golden `event_id`** from fixed fixtures (mirror
   `build_member_invited`'s golden — note it is a regression lock, not a published vector).
2. **Core fold acceptance (reuse existing tests).** Confirm the landed `gate_join` tests in
   `membership/fold.rs` + `tests/membership_fold.rs` cover valid/wrong-identity/wrong-secret/
   expired; add an end-to-end fold test that builds genesis (`build_room_created`) + invite
   (`build_member_invited`) + join (**new** `build_member_joined`) and asserts the joiner is
   `Active` — proving the three builders compose.
3. **Settle the bootstrap approach (OQ1).** Get architect/security sign-off on Approach A vs
   B (§4 D5). The remaining steps assume **A**.
4. **Net: provisional admission.** Add `AdmissionDecision::AdmitProvisional` + a
   provisional-aware admission (open-invite gate) in `admission.rs`; unit-test the decision
   matrix (no open invites ⇒ reject; open invites + unknown device ⇒ provisional; Active ⇒
   admit; Removed/fail-closed ⇒ reject).
5. **Net: bootstrap-scoped accept + upgrade-on-learn.** In `handler.rs`/`node.rs`, accept a
   provisional connection, restrict its service to the membership sub-DAG, accept a single
   `member.joined`, upgrade admission on `Ingest::Accepted`, and close on window-timeout. Add
   stable `join.bootstrap.*` audit lines.
6. **Admin "joinable" affordance (OQ3).** Either a new `room host` command or a
   provisional-aware mode of `room tail` for the admin. Brings up a `Node`, prints
   `listening:`, serves joins (and optionally tails).
7. **CLI join orchestration.** `join.rs::join(...)` per D6: decode + pre-check (D3); open
   store + engine; build joiner admission from the ticket (admin → inviter_identity, Active);
   `Node::spawn`; dial (D4); wait for `Invited` (membership pulled); `select_heads`; build
   binding; `build_member_joined`; stateless self-validate; local fold-check with reason
   mapping (D6 step 10); `publish`; wait for local `Active`; idempotent local insert;
   shutdown; return `JoinSummary`.
8. **CLI wiring + output.** `cli.rs` `RoomAction::Join` (+ optional `Host`) + dispatch;
   `print_join` (D10); `mod join;`.
9. **Tests.** Core (step 1–2); CLI unit (`join_cli.rs`); two-peer integration
   (`join_e2e.rs`, §11).
10. **Verify.** `scripts/verify.sh` green (fmt, clippy `-D warnings`, all tests).
11. **Docs (flag, don't silently skip).** Reconcile `docs/getting-started.md` Step 3 join
    half + the admin "joinable" step + (if added) `room host`. Rides the docs-conformance
    flow; note in the PR (OQ6).

---

## 11. Test strategy

Mapping the issue Test Plan ("two-peer CLI integration test with valid join, wrong identity,
expired invite, and bad secret") to concrete tests:

**Core unit (`event/join.rs`):**
- `build_member_joined` deterministic; content round-trips (incl. `display_name`
  present/absent); signature verifies under `device_id`; `prev_events` preserved; built join
  passes stateless `validate_wire_bytes`; golden `event_id` regression lock.

**Core fold integration (`membership/fold.rs` / `tests/membership_fold.rs`):**
- The three-builder compose test (step 2): genesis + invite + `build_member_joined` ⇒ joiner
  `Active`. Reuse the landed `gate_join` reject tests for wrong-identity / wrong-secret /
  expired (already present; extend to use the new builder where convenient).

**Net unit (`admission.rs`, Approach A):**
- Decision matrix: no open invites ⇒ reject before bytes; open invites + unknown device ⇒
  `AdmitProvisional`; Active member ⇒ `Admit`; Removed / fail-closed ⇒ reject.

**CLI unit (`join.rs` `#[cfg(test)]` / `tests/join_cli.rs`, no network):**
- Ticket decode failures (bad prefix/checksum/truncation) → actionable error, no IO.
- Wrong-identity pre-check → error referencing both ids, no IO (AC: wrong identity).
- `--timeout` / `--peer` parse errors → error before IO.
- No-identity / unknown-flag → error.

**Two-peer integration (`net/tests/join_e2e.rs`, `NetMode::Loopback`, the headline AC5):**
Set up the admin with genesis (`build_room_created`) + a `build_member_invited` for the
joiner's key; bring up the admin in the joinable/provisional mode and the joiner via the
`join` orchestration (or its library entry point), exchanging `listening:` addresses as
`--peer`:
- **Valid join:** the joiner becomes `Active`; after sync, **`room members` on both peers**
  lists the joiner `active` (AC5). Assert via `Node::snapshot()` / the store fold on both.
- **Wrong identity:** a join attempted by a non-invited key is rejected; the admin's snapshot
  never shows it `Active`; the CLI errors.
- **Expired invite:** invite with past `expires_at` ⇒ `ExpiredInvite` on both peers,
  deterministically (independent of wall-clock); joiner not `Active` anywhere (AC: expired
  deterministic).
- **Bad secret:** a join with a wrong `capability_secret` ⇒ `BadCapability`; joiner not
  `Active`; the CLI errors (AC: bad secret).
- Every await is timeout-bounded (mirror `pipe_e2e`/`message_e2e`); `IROH_ROOMS_HOME` cleared
  and `--data-dir` isolation per peer.

**CLI binary integration (`assert_cmd`, optional but recommended):** drive two `iroh-rooms`
processes over loopback (admin `room host`/`room tail` + joiner `room join`), assert exit
codes, the `joined:` stdout line, no secret in output, and `room members` agreement on both.

---

## 12. Risks & mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| **Bootstrap admission weakens the "no bytes to non-members" perimeter** | **high** | Approach A scoped to open-invite windows, membership-sub-DAG-only service, bounded window, audit lines; authorization unchanged (`gate_join` decides everywhere). Approach B removes it entirely. **OQ1 sign-off required.** |
| Joiner can't reach the admin (admin offline / NAT) | medium | `--peer` for deterministic dials; clear bootstrap-timeout error; real-NAT is the open Gate-A risk (#9), not regressed here. |
| Capability secret on the join log surprises reviewers | low (by design) | Document it is the protocol (Spike §7), key-bound + departure-consumed; CLI never prints it. |
| Pull-before-build ordering race (build before membership arrives) | medium | Step 5 blocks on `status(self) == Invited` before building; step 10 fold-check fails closed if ancestors are incomplete (`Buffered`/`Rejected`). |
| `prev_events` doesn't descend from the invite | low | Heads are taken **after** the pull, so they include/descend from the invite; bounded to `MAX_PREV_EVENTS`. |
| Local clock makes expiry non-deterministic | low | Expiry is log-only signed-fields; local clock only advisory (D8). |
| New admin command (`room host`) scope creep | low | Prefer reusing `room tail` with a provisional-aware gate; settle in OQ3. |
| Ticket corruption on paste | low | `RoomInviteTicket::FromStr` is versioned + BLAKE3-checksummed, already fails closed. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Where satisfied | Test |
|---|---|---|
| Valid invited identity can join | `build_member_joined` + `gate_join` accept; bootstrap delivers it to the admin | core compose test; `join_e2e` valid case (both peers `Active`) |
| Wrong identity key cannot use the ticket | CLI key-binding pre-check (D3) + `gate_join` key-binding (`BadCapability`) | CLI pre-check unit test; fold `join_by_wrong_identity_rejected`; `join_e2e` wrong-identity |
| Bad capability secret is rejected | `gate_join` hash recompute (`BadCapability`) | fold `join_with_wrong_capability_secret_rejected`; `join_e2e` bad-secret |
| Expired invite is rejected deterministically | log-only signed-fields expiry in `gate_join` (D8) | fold `join_after_invite_expiry_rejected`; `join_e2e` expired (both peers, clock-independent) |
| Joined member appears in `room members` on both peers after sync | join published → admin folds + persists → both fold `Active` (AC5) | `join_e2e` valid case asserts `room members` agreement on both peers |

---

## 14. Dependencies & sequencing

- **Hard deps (landed):** #18 (`RoomInviteTicket`, `build_member_invited`), #12 (`gate_join`,
  fold), #11 (`SyncEngine`, `WantMembership` never-windowed pull). Transitively #9 (`Node`,
  admission) and #20 (online orchestration helpers). All present in `main`.
- **Reuses (landed):** IR-0002 event model + `MemberJoined` + `verify_bindings`, IR-0101
  identity/secret loader + clock, IR-0102/0103/0105 CLI patterns.
- **New surface owned here:** `build_member_joined` (core), `room join` (cli), the provisional
  bootstrap admission (net), and the admin "joinable" affordance.
- **Enables:** the full two-human exchange in `docs/getting-started.md` (Step 3 → Step 4
  messaging round-trip, currently gated on #19 per the README); subsequent agent-join (#?).

---

## 15. Assumptions

1. The joiner already ran `identity create`, and that identity equals `ticket.invitee_key`
   (the admin invited *this* key). A mismatch is the AC "wrong identity" case.
2. The admin is **online and hosting joins** during the bootstrap (it provably holds the
   membership sub-DAG, being the inviter). MVP bootstraps from the single admin only.
3. The `member.joined` schema, `DeviceBinding`, `capability_hash`, and `gate_join` are final
   and unchanged by this issue (confirmed in `content.rs`, `binding.rs`, `fold.rs`).
4. The engine's `on_connect` `WantMembership` pull + `serve_want_membership` already deliver
   the never-windowed genesis + invite + admin chain; no new sync message types are needed
   for the pull (Approach A; Approach B adds a join-handshake message on a new ALPN).
5. `created_at = clock::now_ms()` is an acceptable signed anchor for the join; expiry is
   enforced log-only against `invite.expires_at` (Spike §6).
6. The capability secret is exactly 16 bytes (matches `MemberJoined.capability_secret:
   bstr[16]` and the ticket).
7. Loopback two-peer testing (as in `message_e2e`/`pipe_e2e`) is the acceptance vehicle;
   real-NAT (Gate A) is tracked separately under #9.

---

## 16. Open questions

- **OQ1 — Bootstrap admission design (BLOCKING, security).** Approach A (provisional
  open-invite admission, recommended for the MVP slice; privacy trade-off) vs Approach B
  (dedicated capability-proving join ALPN, preserves the perimeter; more net protocol). This
  is the make-or-break decision; needs architect + security sign-off before the net work
  starts. §4 D5.
- **OQ2 — Self-contained ticket (Approach C).** Should the ticket embed the genesis + invite
  (or invite `event_id`) so the join can be built offline and the bootstrap collapses to a
  single push? Cleaner joiner, but changes the landed #18 ticket format. Recommend deferring.
- **OQ3 — Admin "joinable" affordance.** New `room host <ROOM_ID>` command vs a
  provisional-aware mode of the admin's `room tail` (`--accept-joins`). Recommend the smaller
  `room tail` change unless a dedicated host UX is wanted.
- **OQ4 — Bootstrap from a non-admin active member.** Allow any `Active` member who holds the
  sub-DAG to host a join (resilience if the admin is offline), or admin-only for MVP?
  Recommend admin-only for MVP (the ticket's discovery hint is the admin).
- **OQ5 — `--display-name`.** Confirm the flag name and bound; confirm it is purely advisory
  display metadata (it is — `member.joined.display_name`, non-authoritative).
- **OQ6 — Docs reconcile.** `docs/getting-started.md` Step 3 join half + the admin joinable
  step + (if added) `room host`. Owned by the docs-conformance flow; flagged so it is not
  missed.
- **OQ7 — `room join` confirmation semantics.** Should success require confirmation that the
  *admin* persisted the join (round-trip), or only that the *joiner* reached local `Active`
  + published? Recommend best-effort admin confirmation within `--timeout`, reporting (not
  failing) if the admin's ack is not observed — consistent with the §16.3 availability model
  — while still guaranteeing the join is stored locally.
```
