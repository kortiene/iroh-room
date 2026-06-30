# Spec: Room Create with `room.created` Event (IR-0102)

| | |
|---|---|
| **Issue** | #17 — `[IR-0102] Implement room create with room.created event` |
| **Parent epic** | #2 |
| **Labels** | `type/feature` `area/protocol` `area/storage` `area/cli` `priority/p0` `risk/medium` |
| **Dependencies** | #16 (IR-0101, identity & device CLI — **landed**), #6 (IR-0002, canonical signed event model — **landed**), #8 (IR-0004, SQLite event store — **landed**), #12 (IR-0008, membership fold — **landed**) |
| **Traceability** | PRD `PRD.v0.3.md` §15.2, §11.1, §16; Spike `PHASE-0-SPIKE.md` Event Protocol §5 (Room ID Derivation), §7 (`room.created`); Membership & Ordering §3.2 |
| **Owning crates** | `crates/iroh-rooms-cli` (command surface + orchestration) + one small additive genesis builder in `crates/iroh-rooms-core::event` |

> **Status:** landed (issue #17 / IR-0102). The compiled binary is the source of truth;
> this document is the build plan and design record.

---

## 1. Summary

Add the second subcommand group to the `iroh-rooms` binary: **room creation**.

```bash
iroh-rooms room create "MX-Loom Build Room"
```

This command:

1. loads the local identity created by `iroh-rooms identity create` (#16),
2. assembles a genesis `room.created` event (Spike Event Protocol §7) whose
   content names the creator as the **sole immutable admin** and carries the
   creator's room-scoped **device binding**,
3. derives the `room_id` from the genesis fields (§5),
4. signs the event under the creator's **device key** (§6),
5. **persists** the verbatim event into the local SQLite event store (#8), and
6. prints the new `room_id` plus actionable next steps.

Every cryptographic and storage primitive this needs already exists and is
conformance-tested: the `room.created` content type and the room-id / binding /
signing / sealing derivations in `iroh-rooms-core::event` (#6), the idempotent
SQLite event store in `iroh-rooms-core::store` (#8), and the deterministic
membership fold in `iroh-rooms-core::membership` (#12). This issue is the **thin
orchestration layer** that stitches them into a CLI command, plus two small,
clearly-scoped additions:

- a **secret-key loader** in the CLI's `identity` module (today only the *public*
  profile is loadable — signing requires the secret seeds), and
- a **pure genesis builder** in `iroh-rooms-core::event` so the byte-exact
  assembly of `room.created` lives in one tested place reused by later flows.

The "room" and its "membership" are **not** stored as their own tables: the
append-only event log is the single source of truth, and the room + membership
state are **derived by folding** the persisted genesis (PRD §12, Spike §9). This
keeps the change minimal and architecturally aligned — "Room survives CLI
restart" is satisfied because the genesis event persists and re-folds
deterministically (#8 `rebuild()` / #12 fold).

---

## 2. Background & current repository state

### 2.1 What exists (landed work this builds on)

- **Event model (`iroh-rooms-core::event`, #6).** The `room.created` content type,
  CBOR codec, room-id / event-id derivations, device binding, signing, the
  `WireEvent` envelope, and the stateless `validate_wire_bytes` pipeline. Exact
  surfaces used by this issue (all `pub`):
  - `event::content::RoomCreated { room_name: String, room_nonce: [u8;16], admins: Vec<IdentityKey>, device_binding: DeviceBinding }` and `Content::RoomCreated(..)` (`crates/iroh-rooms-core/src/event/content.rs:129`).
  - `event::signed::SignedEvent` (the eight signed fields) with `to_csb()`
    (`signed.rs:45`) and `event_id()` (`signed.rs:84`).
  - `event::signed::derive_room_id(creator_sender_id: &IdentityKey, room_nonce: &[u8;16], created_at: u64) -> RoomId` (`signed.rs:236`) — the §5 derivation.
  - `event::signed::sign_csb(csb: &[u8], device_secret: &SigningKey) -> Signature` (`signed.rs:261`).
  - `event::binding::DeviceBinding::create(room_id: &RoomId, identity_secret: &SigningKey, device_key: DeviceKey) -> DeviceBinding` (`binding.rs:51`).
  - `event::wire::WireEvent::seal(signed: Vec<u8>, sig: Signature) -> WireEvent` and `to_bytes()` (`wire.rs:42`, `wire.rs:61`).
  - `event::validate::validate_wire_bytes(bytes, &ValidationContext) -> Result<ValidatedEvent, RejectReason>` and `ValidationContext::for_room(RoomId)` (`validate.rs:79`, `validate.rs:41`). For `room.created` this does the **full** check — recomputes `room_id` (§6 step 6), verifies the binding (step 7), verifies the signature under `device_id` (step 3), and enforces `prev_events == []` (step 9a). Genesis is membership-exempt (step 8), so **no oracle is needed**.
  - `event::keys::SigningKey` with `from_seed(&[u8;32])`, `to_seed() -> Zeroizing<[u8;32]>`, `identity_key()`, `device_key()`, `sign()` (`keys.rs:224`).
  - `event::ids::RoomId` implements `Display`/`FromStr` as `blake3:<64-hex>` (`ids.rs:107`) — the CLI presentation and argument form.
- **SQLite event store (`iroh-rooms-core::store`, #8, behind the `store` feature).**
  - `EventStore::open(path: &Path) -> Result<Self, StoreError>` — opens/creates the DB and applies the schema (`store/mod.rs:73`).
  - `EventStore::insert(&mut self, ev: &ValidatedEvent) -> Result<InsertOutcome, StoreError>` — idempotent persist with a `BLAKE3(wire.signed) == event_id` integrity guard; returns `Inserted | Duplicate` (`store/mod.rs:105`).
  - Read surface for inspection: `room_event_ids(&RoomId)`, `get(&EventId)`, `by_type(&RoomId, EventType)`, `count(&RoomId)`.
- **Membership fold (`iroh-rooms-core::membership`, #12, always compiled, no feature).**
  - `RoomMembership::from_events(room_id: RoomId, events: impl IntoIterator<Item = ValidatedEvent>) -> Self` and `snapshot() -> MembershipSnapshot` (`membership/fold.rs:127`, `fold.rs:171`).
  - `MembershipSnapshot::admin() -> Option<&IdentityKey>`, `member(&IdentityKey) -> Option<&Member>`, `is_active(&IdentityKey) -> bool`, with `Member { identity, device, status: Status, role: Role }` (`membership/model.rs`).
- **CLI scaffold (`crates/iroh-rooms-cli`, #16).**
  - `cli.rs`: `Cli { data_dir, command }`, `Command::Identity { action }`, `run()` dispatch (`cli.rs:18`, `cli.rs:65`).
  - `paths.rs`: `data_dir(flag) -> PathBuf` (flag > `IROH_ROOMS_HOME` > platform default), `ensure_dir(&Path)` (creates the home `0700` on Unix) (`paths.rs:31`, `paths.rs:64`).
  - `identity.rs`: `Profile { version, name, identity_id, device_id, created_at_ms }` and `Profile::load(home) -> Result<Profile>` — **reads only `identity.json` (public); the secret file is never opened** (`identity.rs:42`, `identity.rs:64`). On-disk secret file `identity.secret` is `{"version":1,"identity_secret":"<64hex>","device_secret":"<64hex>"}`.
  - `main.rs`: maps `Err` → `eprintln!("error: {err:#}")` + `ExitCode::FAILURE` (`main.rs:12`).

### 2.2 The two real gaps (this issue closes both)

1. **The CLI cannot persist events.** `crates/iroh-rooms-cli/Cargo.toml` depends on
   `iroh-rooms-core` **without** enabling the `store` feature, so `EventStore` is
   not even compiled into the binary. `room create` must enable it (D2).
2. **The CLI cannot load the signing secrets.** `Profile::load` deliberately reads
   only the public `identity.json` (#16 D8). Signing a `room.created` needs both
   secret seeds (the device key signs the event; the identity key signs the
   binding). A new secret-loading function is required (D4).

### 2.3 Spike facts that constrain the design

- **`room_id` derivation (Event Protocol §5):**
  `room_id = BLAKE3-256("iroh-rooms:room-id:v1" ‖ creator_sender_id(32) ‖ room_nonce(16) ‖ u64_be(created_at))`.
  A peer recomputes and **rejects** a `room.created` whose envelope `room_id`
  disagrees (`room_id_mismatch`). Golden vector: creator seed `0x01×32`,
  `room_nonce = 000102…0e0f`, `created_at = 1750000000000` ⇒
  `room_id = 43c19f2e…16a3`.
- **`room.created` content (Event Protocol §7):**
  ```
  content = {
    "room_name":      tstr,
    "room_nonce":     bstr[16],          // feeds room_id (§5)
    "admins":         [ bstr[32], ... ], // MUST be exactly [sender_id] in MVP
    "device_binding": DeviceBinding      // identity_key == sender_id, device_key == device_id
  }
  ```
  Validate: `prev_events == []`; `room_id` recomputes; `sender_id ∈ admins`;
  binding verifies. (`admins.len() == 1 && admins[0] == sender_id` is enforced by
  the landed `content::check_field_rules`.)
- **Device binding (Event Protocol §1):**
  `binding_msg = "iroh-rooms:device-binding:v1" ‖ room_id(32) ‖ sender_id(32) ‖ device_id(32)`,
  `binding_sig = Ed25519_sign(identity_secret, binding_msg)`. It is **room-scoped**,
  so it can only be built *after* `room_id` is derived (hence it is produced here,
  not at identity-creation time — #16 §2 explicitly deferred it to this issue).
- **Single immutable admin (Membership §3.1/§3.2):** the genesis signer is the sole
  authority for the whole MVP. `room.created` sets `admin = signer`, initial members
  `= {creator: Active}`. There is no admin transfer or co-admin.
- **Signing model (§1/§6):** events are signed by `device_id` (the iroh
  `EndpointId`), authorized against `sender_id`. The signature MUST verify under
  `device_id`, never `sender_id`.
- **Persistence (PRD §12, Spike §9):** the append-only `events` log is the single
  source of truth; `members`/room state are **derived caches** rebuildable by
  re-folding the log. No new tables are introduced by this issue.

### 2.4 Workspace conventions to honor

- Lints: `unsafe_code = "forbid"`, `clippy::all` + `clippy::pedantic = warn`
  (workspace). New code must be clippy-pedantic clean.
- `scripts/verify.sh` runs fmt + clippy + tests **`--all-features`** across the
  workspace and is the CI gate.
- Deps are added deliberately with an explanatory comment (house style in the
  existing `Cargo.toml`s).
- Secret hygiene (#16 D8): secret bytes live only inside `SigningKey` / `Zeroizing`
  buffers; never in any `Debug`/`Display`/`format!`/log/error path.

---

## 3. Goals, non-goals, scope

### 3.1 In scope

1. `iroh-rooms room create <name>` — assemble, sign, validate, and persist a
   genesis `room.created`; print the `room_id`, the admin, and next steps.
2. A **minimal inspection command**, `iroh-rooms room members <room-id>`, that
   re-derives the room's membership from the persisted log and prints the
   admin/members. Required so the black-box CLI integration test can "inspect
   persisted room and members" (issue Test Plan) and to satisfy AC4/AC5 through the
   CLI surface. (See OQ-1 for `room list`/`room show`.)
3. A secret-key loader in `identity.rs` (`load_secret` / `SecretKeys`) reading
   `identity.secret`, with full secret hygiene (D4).
4. A pure, deterministic genesis builder in `iroh-rooms-core::event` (D1).
5. Enable the `store` feature on the CLI's `iroh-rooms-core` dependency (D2).
6. Tests: CLI integration tests against a temp `IROH_ROOMS_HOME`, plus core unit
   tests for the genesis builder (incl. the §5 golden `room_id`).

### 3.2 Out of scope (sibling issues — do **not** implement here)

- Invites, joining, removal (`member.invited/joined/left/removed`) — #3 and later.
- Messaging, files, pipes, agent status — later issues.
- Any networking / iroh transport / sync — #9 / #11 (landed prototypes, not wired
  to this command).
- New SQLite tables (`rooms`, `members`, …): membership stays a **derived** fold
  over the event log (PRD §12, Spike §9). We do not denormalize a `rooms`/`members`
  table in this issue.
- Multi-admin, admin transfer, key rotation, multi-device (Spike §0/§3.1; PRD §13.4).
- A full `room tail` / rich `room members` presentation — only the minimal
  inspection needed for the acceptance criteria lands here.

### 3.3 Why the split is safe

Room creation depends only on the landed event model (#6), the landed store (#8),
the landed fold (#12), and the local identity (#16). No protocol rule is
re-decided here: `validate_wire_bytes` is the single judge of the event we build,
and the fold is the single judge of "creator is admin". The new code is pure glue
plus two small primitives (a secret loader; a deterministic builder), each
independently testable.

---

## 4. Key design decisions

### D1 — A pure genesis builder lives in core (recommended)

Add one additive, **deterministic** function to `iroh-rooms-core::event` (suggested
module `event::genesis`, re-exported at `event::build_room_created`):

```rust
/// Assemble and sign a genesis `room.created` event (Event Protocol §5/§6/§7).
///
/// Pure and deterministic in its inputs: the caller injects `room_nonce`
/// (CSPRNG) and `created_at` (clock) so this stays clock-/RNG-free and
/// golden-testable. Returns the sealed `WireEvent`; the caller validates,
/// persists, and reads `room_id` back via `validate_wire_bytes`.
#[must_use]
pub fn build_room_created(
    identity_secret: &SigningKey,
    device_secret: &SigningKey,
    room_name: &str,
    room_nonce: &[u8; SHORT_ID_LEN],   // 16
    created_at: u64,
) -> WireEvent
```

Internals (exactly the existing fixture pattern in
`crates/iroh-rooms-core/tests/membership_fold.rs`):

1. `sender_id = identity_secret.identity_key()`, `device_id = device_secret.device_key()`.
2. `room_id = derive_room_id(&sender_id, room_nonce, created_at)`.
3. `binding = DeviceBinding::create(&room_id, identity_secret, device_id)`.
4. `content = Content::RoomCreated(RoomCreated { room_name: room_name.to_owned(), room_nonce: *room_nonce, admins: vec![sender_id], device_binding: binding })`.
5. `event = SignedEvent { schema_version: 1, room_id, sender_id, device_id, event_type: EventType::RoomCreated, created_at, prev_events: vec![], content }`.
6. `csb = event.to_csb()`; `sig = sign_csb(&csb, device_secret)`; `WireEvent::seal(csb, sig)`.

- **Why core, not CLI:** the byte-exact genesis assembly is protocol logic. Keeping
  it in core gives it a single conformance test (the §5 golden `room_id`), keeps it
  reusable by future flows and the `net` crate, and mirrors the IR-0101 precedent
  (key *generation* went to core; IO stayed in the CLI). It adds **no** new
  dependency to core (everything used is already in `event`).
- **Why pure (inject nonce + clock):** core stays free of wall-clock and RNG in its
  builders (the only RNG in core is `SigningKey::generate`), so `build_room_created`
  is fully deterministic and the golden-vector test needs no time/seed mocking.
- **Alternative considered (rejected):** assemble inline in the CLI using the
  existing `event` primitives. Works and is only ~10 lines, but spreads protocol
  byte-assembly into the app layer and loses the core-level golden test. Acceptable
  fallback if a reviewer prefers zero core change (see OQ-3).

### D2 — The CLI enables the `store` feature on core (required)

```toml
# crates/iroh-rooms-cli/Cargo.toml
iroh-rooms-core = { path = "../iroh-rooms-core", features = ["store"] }
```

Without this, `EventStore` is not compiled into the binary and `room create`
cannot persist. The `membership` module is always compiled (no feature), so the
inspection command needs nothing further. `scripts/verify.sh --all-features`
already exercises `store`, so no CI change is needed.

### D3 — Store lives at `<HOME>/rooms.db`; one store, many rooms

- The event store is opened at `paths::data_dir(..)`-resolved `<HOME>/rooms.db`
  (the path IR-0004 reserved for it; #16 D3 established one home per participant).
  `paths::ensure_dir(&home)` (0700 on Unix) is called before opening so the DB and
  its WAL sidecars (`rooms.db-wal`, `rooms.db-shm`) are created inside an
  owner-only directory.
- **One store holds many rooms.** Unlike identity, `room create` is **not** a
  clobber operation: each invocation generates a fresh `room_nonce` and
  `created_at`, derives a new `room_id`, and **appends** a new genesis. Re-running
  `room create "X"` makes a *second* room. (Idempotency is per `event_id`: the
  store's `insert` returns `Duplicate` only for a byte-identical event, which never
  happens across two creates because the nonce differs.)
- **DB-file permissions:** the 0700 home already restricts access. Setting the DB
  file itself to `0600` on Unix is a cheap best-effort hardening but is **not**
  required for correctness (the dir gates it). See OQ-2.

### D4 — Secret-key loader in `identity.rs`, with strict hygiene

Add to `crates/iroh-rooms-cli/src/identity.rs`:

```rust
/// The two secret signing keys backing the local identity. Never printed,
/// logged, or serialized; the seeds live only inside `SigningKey`.
pub struct SecretKeys {
    pub identity: SigningKey,   // signs the device binding
    pub device:   SigningKey,   // signs the event (device_id)
}

/// Load the secret seeds from `<home>/identity.secret`.
///
/// # Errors
/// Actionable error if no identity/secret file exists (points at
/// `iroh-rooms identity create`), or if the file is unreadable/corrupt.
pub fn load_secret(home: &Path) -> Result<SecretKeys>
```

- Parse the JSON into a serde struct **without** a leaking `Debug`; hold each hex
  field's decoded bytes in `Zeroizing<[u8; 32]>`; build each `SigningKey` via
  `from_seed`; zeroize all intermediates. Reuse `hex` (already a dep).
- **Consistency guard (recommended):** after loading, also `Profile::load(home)` and
  assert the derived public keys equal the stored `identity_id` / `device_id`. A
  mismatch is a hard error ("identity files are inconsistent or corrupt") — the same
  tamper/corruption guard the identity spec applies on the public side (#16 §8).
- The missing-file error must mirror `Profile::load`'s wording so a user who never
  ran `identity create` gets the same actionable hint.

### D5 — `room create` orchestration (new `crates/iroh-rooms-cli/src/room.rs`)

```rust
pub fn create(home: &Path, name: &str) -> Result<RoomCreatedSummary>
```

Flow:

1. `validate_room_name(name)` (D7) — fail before any IO so nothing is written.
2. `let secret = identity::load_secret(home)?;` (also yields the public profile via
   the consistency guard, giving us `identity_id` for the summary).
3. `paths::ensure_dir(home)?;` then generate inputs:
   - `room_nonce: [u8; 16]` from the OS CSPRNG (`getrandom::fill`; add `getrandom`
     to the CLI crate — already in the lockfile),
   - `created_at = now_ms()` (ms epoch; reuse/extract the existing private `now_ms`
     helper from `identity.rs` into a shared `util`/`clock` — see OQ-4).
4. `let wire = event::build_room_created(&secret.identity, &secret.device, name, &room_nonce, created_at);`
5. `let validated = validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))?;`
   where `room_id = derive_room_id(&secret.identity.identity_key(), &room_nonce, created_at)`.
   This is a **belt-and-suspenders self-check**: it re-derives `room_id`, verifies
   the binding, verifies the signature under `device_id`, and confirms
   `prev_events == []` before we touch disk. (For our own freshly-built event it
   must pass; a failure is an internal bug, surfaced as an error, never a silent
   persist.)
6. `EventStore::open(&home.join("rooms.db"))?.insert(&validated)?;`
7. Return `RoomCreatedSummary { room_id, room_name, admin_identity_id }`.

The CLI dispatch prints (script-friendly, deterministic order):

```text
created room "MX-Loom Build Room"
room_id: blake3:43c19f2e…16a3
admin: 4f3c…(64-hex identity_id)
next: run `iroh-rooms room members blake3:43c19f2e…16a3`
```

### D6 — `room members <room-id>` derives membership by folding the log

```rust
pub fn members(home: &Path, room_id: &RoomId) -> Result<()>
```

1. `EventStore::open(&home.join("rooms.db"))?`.
2. Collect the room's events: `store.room_event_ids(room_id)?` then `store.get(id)?`
   for each (or `by_type` per type). If the set is empty, error with an actionable
   "no room <id> in this data directory" message.
3. Re-validate each stored event: `validate_wire_bytes(&stored.wire.to_bytes(), &ValidationContext::for_room(*room_id))?`.
4. Fold: `let snap = RoomMembership::from_events(*room_id, validated_events).snapshot();`.
5. Print the admin and each member's `identity_id`, `role`, `status`
   (deterministic identity order via `snap.members()`). For a freshly-created room
   this is exactly one row: the creator, `Admin`, `Active`.

This command is what makes AC4 ("creator appears as admin in membership state") and
AC5 ("survives CLI restart") observable through the black-box CLI, and is the
"inspect persisted room and members" step in the issue Test Plan. It introduces no
new protocol logic — it is a read-only re-fold of the landed store + membership.

### D7 — Room-name validation

`RoomCreated.room_name` is `tstr` with **no explicit bound** in the spike. Apply a
conservative local bound: **1..=128 UTF-8 bytes, no control characters** (so it
stays clean in `members` output and CBOR content). Empty or control-bearing names
are rejected before any IO. (The 128-byte choice is a proposal — see OQ-5;
`message.text.body` is capped at 16384, room names should be far shorter.)

### D8 — Errors → stderr + non-zero exit; success → stdout + exit 0

Reuse the established `main.rs` mapping (`error: {err:#}` + `ExitCode::FAILURE`).
No secret bytes ever reach an error path (the secret loader holds seeds in
`Zeroizing` and never formats them).

---

## 5. CLI surface (precise)

```text
iroh-rooms [--data-dir <PATH>] room create <NAME>
iroh-rooms [--data-dir <PATH>] room members <ROOM_ID>
```

- `room create <NAME>` — `NAME` is a positional arg (matches PRD §16
  `iroh-rooms room create "Project Room"`). 1..=128 UTF-8 bytes, no control chars.
- `room members <ROOM_ID>` — `ROOM_ID` is the `blake3:<64-hex>` string printed by
  `create`, parsed via `RoomId::from_str`.
- Wiring in `cli.rs`: add `Command::Room { action: RoomAction }` and
  `enum RoomAction { Create { name: String }, Members { room_id: String } }`,
  modeled exactly on the existing `Identity { action }`. The global `--data-dir`
  already applies to all subcommands.

---

## 6. Module / file plan

```text
crates/iroh-rooms-cli/
  Cargo.toml          # enable core `store` feature; + getrandom (nonce); (deps below)
  src/cli.rs          # + Command::Room { action }, RoomAction, dispatch arms
  src/room.rs         # NEW: create(), members(), RoomCreatedSummary, name validation
  src/identity.rs     # + load_secret() -> SecretKeys (secret loader, D4)
  src/main.rs         # + `mod room;` (no other change)
  tests/room_cli.rs   # NEW: integration tests (assert_cmd) per §11

crates/iroh-rooms-core/
  src/event/genesis.rs  # NEW: build_room_created() (D1), re-exported in event/mod.rs
  src/event/mod.rs      # + `pub mod genesis;` + re-export build_room_created
```

No change to `paths.rs` beyond reuse. If `now_ms` is extracted to a shared helper
(OQ-4), add `src/util.rs` (or `clock.rs`); otherwise duplicate the 5-line helper in
`room.rs`.

---

## 7. Dependencies to add

CLI crate (`crates/iroh-rooms-cli/Cargo.toml`):

| Crate / change | Why |
|---|---|
| `iroh-rooms-core` **`features = ["store"]`** | Compile `EventStore` into the binary so `room create` can persist (D2). |
| `getrandom` | OS CSPRNG for the 16-byte `room_nonce` (already in the workspace lockfile). |

No new dev-deps: `assert_cmd`, `predicates`, `tempfile`, `serde_json` are already
present. Core needs **no** new dependency (the genesis builder uses only existing
`event` types). Each addition gets a one-line house-style comment.

---

## 8. Error model & observability

| Condition | Behavior |
|---|---|
| `room create`/`members` with no identity (no `identity.secret`) | stderr: "no identity in `<HOME>`; run `iroh-rooms identity create --name <name>`"; non-zero exit; nothing written. |
| `identity.secret` corrupt / public-private mismatch | stderr: "identity files are inconsistent or corrupt"; non-zero exit. |
| `room create` name invalid (empty / >128 bytes / control char) | stderr explains the bound; non-zero exit; **no** event persisted. |
| `<HOME>/rooms.db` not openable / not writable | stderr with path + the `StoreError`; non-zero exit. |
| Self-validation of our own genesis fails (internal bug) | stderr with the `RejectReason` code; non-zero exit; **not** persisted. |
| `room members <ROOM_ID>` with an unparsable id | stderr: "invalid room id (expected `blake3:<hex>`)"; non-zero exit. |
| `room members <ROOM_ID>` for a room not in this store | stderr: "no room `<id>` in `<HOME>`"; non-zero exit. |

Conventions: success → stdout + exit 0; all errors → stderr + non-zero (single
generic code 1 for MVP, as identity). No logging framework, no telemetry — output
is the only observability surface and is deliberately minimal and secret-free.

---

## 9. Security, privacy, reliability

- **Sign under `device_id`, bind under `sender_id`.** The event signature is
  produced with the device secret (`sign_csb`); the device binding is produced with
  the identity secret (`DeviceBinding::create`). `validate_wire_bytes` re-checks
  both before persist, so a wrong-key assembly fails closed (vector §5: verifying
  under `sender_id` is `bad_signature`).
- **Single immutable admin.** `admins = [sender_id]` is enforced both at build (D1)
  and at validate (`check_field_rules`), and the fold sets the creator as the sole
  `Admin` (Membership §3.1). There is no path to a second admin in this command.
- **Secret hygiene.** Seeds are read into `Zeroizing` buffers, used to build
  `SigningKey`s, and wiped; no secret bytes appear in any output or error (regression
  test in §11 greps all output for the seed hex). The `SecretKeys` struct has no
  leaking `Debug`.
- **At-rest model (MVP).** The genesis event is **public-by-design** signed data
  (it travels to peers); persisting it in plaintext SQLite under the 0700 home is
  correct. No secret material is stored in `rooms.db`. (Identity secrets remain only
  in `identity.secret`.)
- **Reliability / determinism.** `insert` commits in a transaction with a
  `BLAKE3(wire.signed) == event_id` integrity guard; a crash mid-create leaves
  either no genesis or a complete one. The only non-determinism is the injected
  `room_nonce` and `created_at`; everything downstream (room_id, signing, fold) is a
  pure function of signed bytes, so a restart re-derives identical room + membership
  state (#8 `rebuild()` / #12 fold) — the basis for AC5.

---

## 10. Implementation steps (for the executing engineer/agent)

1. **Core builder (D1).** Add `crates/iroh-rooms-core/src/event/genesis.rs` with
   `build_room_created(..)`; `pub mod genesis;` + re-export in `event/mod.rs`. Unit
   tests: (a) the §5 **golden** `room_id` for seed `0x01×32` / nonce `000102…0e0f` /
   `created_at = 1750000000000` equals `43c19f2e…16a3`; (b) `validate_wire_bytes`
   **accepts** the built event under `ValidationContext::for_room(room_id)`;
   (c) tampering `room_name` after build changes `event_id` and the signature still
   verifies (sanity); (d) `admins == [sender_id]` round-trips.
2. **CLI deps (D2).** Enable `features = ["store"]` on `iroh-rooms-core`; add
   `getrandom` to `crates/iroh-rooms-cli/Cargo.toml`, each with a comment.
3. **Secret loader (D4).** Add `identity::load_secret(home) -> Result<SecretKeys>`
   with `Zeroizing` seeds, no leaking `Debug`, and the public/secret consistency
   guard. Unit tests: round-trips keys written by `identity::create`; missing-file
   error mirrors `Profile::load`; corrupt secret → error.
4. **`room.rs` (D5/D6/D7).** Implement `validate_room_name`, `create(home, name)`,
   `members(home, room_id)`, and `RoomCreatedSummary`. Generate the nonce via
   `getrandom`, `created_at` via `now_ms`.
5. **`cli.rs` + `main.rs`.** Add `Command::Room { action }`, `RoomAction`, dispatch
   arms that print the §D5/§D6 output; `mod room;` in `main.rs`.
6. **Output.** `create` prints `room_id` (`blake3:<hex>`), admin, and a next-step
   hint (PRD §16 UX req 1). `members` prints admin + member rows.
7. **Tests (§11).** `tests/room_cli.rs`.
8. **Docs (optional).** If this settles any `[reconcile]` marker in
   `docs/getting-started.md` (room-create command shape / room-id format), note it
   for a follow-up doc PR; the doc change itself is separate.
9. **Gate.** `scripts/verify.sh` green (fmt + clippy pedantic + tests
   `--all-features`).

---

## 11. Test strategy

**Core unit tests** (`crates/iroh-rooms-core`, run without features and under
`--all-features`):

- Genesis builder golden `room_id` (§5 vector) and `validate_wire_bytes` acceptance
  (step 1 above). These prove **AC2** (signed by creator device — validation
  verifies the signature under `device_id`) and **AC3** (room_id recomputes from
  genesis fields) at the protocol layer, deterministically.

**CLI integration tests** (`crates/iroh-rooms-cli/tests/room_cli.rs`, `assert_cmd`,
fresh `tempfile` dir exported as `IROH_ROOMS_HOME` per test):

1. **Create prints a room id (AC1).** `identity create --name Alice` then
   `room create "Build Room"` exits 0 and prints a `blake3:`-prefixed `room_id`;
   `rooms.db` exists.
2. **Creator is admin & active (AC4).** `room members <room_id>` prints the
   creator's `identity_id` with role `admin`/`Admin` and status `active`/`Active`,
   and names them as the room admin.
3. **Survives restart (AC5).** A **separate** `room members <room_id>` process
   invocation (new process = "restart") still reports the creator as admin — the
   state came from `rooms.db`, not memory.
4. **Room id recomputes end-to-end (AC3).** `room members` re-validates the stored
   genesis (which recomputes `room_id` via §5) and succeeds; a `members` call with a
   *different* (well-formed but unknown) `room_id` errors with "no room".
5. **No identity → actionable error.** `room create "X"` with no prior
   `identity create` exits non-zero and hints at `identity create`.
6. **Name validation.** Empty / >128-byte / control-char name → non-zero exit, and
   **no** new genesis is persisted (assert `rooms.db` event count unchanged / absent).
7. **Secrets never leak.** Capture stdout+stderr of `room create`; read the seed hex
   from `identity.secret`; assert it appears in neither stream.
8. **Two rooms are independent.** Two `room create` calls in one home yield two
   distinct `room_id`s, each resolvable by `room members` with the creator as admin.
9. **`--data-dir` honored.** `room create` under `--data-dir <A>` writes
   `A/rooms.db` and is invisible to a `members` call pointed at `B`.

Run under `scripts/verify.sh --all-features`.

---

## 12. Risks & mitigations

| # | Risk | Mitigation |
|---|---|---|
| R1 | CLI can't persist because `store` feature isn't enabled. | D2 enables it; §11 test 1 fails loudly if it regresses; CI is `--all-features`. |
| R2 | Signing under the wrong key (identity vs device). | D1 uses `sign_csb(.., device_secret)` and `DeviceBinding::create(.., identity_secret, ..)`; `validate_wire_bytes` re-checks before persist (vector §5 catches wrong-key). |
| R3 | Secret seed leaks via output/error/Debug. | `Zeroizing` seeds, no leaking `Debug` on `SecretKeys`; §11 test 7 greps output for the seed. |
| R4 | `admins` not exactly `[sender_id]` → invalid genesis. | D1 sets it; landed `check_field_rules` enforces it; builder unit test asserts round-trip. |
| R5 | "Room" modeled as a new table, diverging from the derived-fold architecture. | D6/§3.2: no new tables; membership is re-folded from the persisted log (PRD §12, Spike §9). |
| R6 | `room_nonce`/`created_at` collision makes two creates the same room. | Nonce is 16 CSPRNG bytes + ms clock; collision is cryptographically negligible; each create is a new room by design (D3). |
| R7 | DB sidecar files (`-wal`/`-shm`) world-readable. | Created inside the 0700 home; optional 0600 on the DB file (OQ-2). No secrets are in `rooms.db` regardless. |
| R8 | Self-validation rejects our own event (e.g. a builder bug) and we persist garbage. | The flow validates **before** insert and aborts on any `RejectReason`; insert also re-checks `BLAKE3(wire.signed)`. |
| R9 | Room-name bound diverges from a future `display_name`/content rule. | Conservative 1..=128/no-control bound now; flagged OQ-5 for reconciliation. |

---

## 13. Acceptance criteria → coverage

| Issue AC | Covered by |
|---|---|
| Command creates a room ID and prints it. | D5 build + print; §11 test 1. |
| `room.created` is signed by creator device. | D1 `sign_csb(.., device_secret)`; `validate_wire_bytes` verifies under `device_id`; core golden test + §11 (a stored event that re-validates). |
| Room ID recomputes from genesis fields. | D1 `derive_room_id` (§5); `validate_wire_bytes` recompute check; core golden `room_id` test + §11 tests 3/4. |
| Creator appears as admin in membership state. | D6 fold → `snapshot.admin()`/`member().role == Admin, status == Active`; §11 test 2. |
| Room survives CLI restart. | D3 persist to `rooms.db`; D6 re-fold in a fresh process; §11 test 3. |
| Scope: `iroh-rooms room create <name>`; creator becomes single immutable admin; persist in SQLite. | D5 command; `admins == [sender_id]` (D1 + `check_field_rules`); `EventStore::insert`. |

---

## 14. Dependencies & sequencing

- **Hard prerequisites (all landed):** #6 (event model: builder primitives), #8
  (store: `EventStore::open`/`insert`), #12 (fold: admin derivation), #16 (identity
  CLI: `paths`, `identity`, the on-disk keys this signs with). This issue is
  therefore **unblocked**.
- **Unblocks:** `room invite` / `room join` / `room send` / `room members` (full) —
  they reuse `identity::load_secret`, the `<HOME>/rooms.db` store, and the
  genesis/room-id model established here.
- The orchestrator handles all git/GitHub actions; no branch/PR work is part of this
  phase.

---

## 15. Assumptions

1. One identity per data directory (#16) ⇒ `room create` always signs as that
   single local participant; multiple participants = multiple `--data-dir` homes
   (exactly how the getting-started demo runs Alice/Bob/Agent).
2. The room/membership are **derived** from the event log, not stored in dedicated
   tables (PRD §12, Spike §9). Only the genesis event is persisted.
3. One `rooms.db` per home holds many rooms; `room create` always appends a new
   genesis (never clobbers), unlike `identity create`.
4. The genesis `room.created` is public signed data; plaintext-at-rest under the
   0700 home is acceptable (no secret material in `rooms.db`).
5. `getrandom` and the `store` feature build on the workspace toolchain and pass
   clippy-pedantic (both already resolved in `Cargo.lock`).
6. `room_name` 1..=128 UTF-8 bytes / no control chars is an acceptable local bound
   pending a documented protocol bound (OQ-5).

---

## 16. Open questions

- **OQ-1 (inspection surface):** Ship only `room members <room-id>` now, or also a
  `room list` (enumerate genesis events in the store) and/or `room show <room-id>`
  (name + admin + created_at)? Spec proposes `room members` as the minimal surface
  required by the ACs/Test Plan; `room list`/`show` are cheap follow-ons (the PRD
  §16 surface includes `room members`, `room tail`; `room list` is not in §16).
- **OQ-2 (DB perms):** Set `rooms.db` to `0600` on Unix in addition to the 0700
  home, or rely on the directory? Spec relies on the directory (best-effort 0600 is
  optional hardening).
- **OQ-3 (builder home):** Put `build_room_created` in core (recommended, golden
  test + reuse) or assemble inline in the CLI (zero core change)? Spec recommends
  core.
- **OQ-4 (`now_ms` sharing):** Extract `identity.rs`'s private `now_ms` into a shared
  CLI helper (`util`/`clock`) or duplicate the 5 lines in `room.rs`? Cosmetic;
  extraction is slightly cleaner.
- **OQ-5 (room-name bound):** Confirm 1..=128 UTF-8 bytes / no control chars, or pick
  a different cap. No bound is documented in the spike for `room_name`.
- **OQ-6 (output format):** Should `room create`/`room members` gain a `--json` mode
  now (mirroring `identity show --json`) for script consumption, or defer? Spec
  defers; labeled lines satisfy the ACs.
