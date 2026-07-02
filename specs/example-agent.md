# Spec: Add an Example Agent (IR-0304)

| | |
|---|---|
| **Issue** | #39 — `[IR-0304] Add example agent` |
| **Parent** | #4 |
| **Labels** | `type/feature` `type/docs` `area/agent` `area/dx` `priority/p1` `risk/medium` |
| **Dependencies** | #31 (IR-0206 agent identity/`agent invite` — **landed**), #32 (IR-0207 agent-invite flow conformance — **landed**), #33 (IR-0208 `agent status` — **landed**), #36 (IR-0301 Rust SDK façade `crates/iroh-rooms` — **landed**) |
| **Traceability** | `PRD.v0.3.md` §3.3 Initial Beachhead, §15.8 (agent status), §19 Phase 2 deliverable 8 ("example agent"); builds on §19 deliverable 1 ("Rust SDK", IR-0301). The seed for this work is the doc header of `crates/iroh-rooms/examples/07_agent_status.rs`, which names itself "the seed for PRD §19 Phase 2 deliverable 8, 'example agent'". |
| **Owning crate** | `crates/iroh-rooms` (the public Rust SDK façade). New `examples/` target + a co-located README + one new integration test + (optional, additive) doc pointers. **No production/runtime code changes** in any crate. |

> **Status:** planned — this document is the build plan. It adds a runnable *example* and its
> tests/docs; it does **not** add or change any protocol, event type, authorization primitive, CLI
> command, or library surface. Every capability it exercises already shipped (see §2). The compiled
> crates remain the source of truth for what exists.

---

## 1. Summary

Iroh Rooms now has, end-to-end, everything an autonomous agent needs to participate in a room as a
first-class-but-least-privileged principal: local identity (`identity create`, IR-0101), a key-bound
agent invite (`agent invite`, IR-0206), the join handshake (`room join`, IR-0104), and a signed
`agent.status` event (`agent status`, IR-0208). Phase 2 ("Developer Preview") also shipped the
public Rust SDK façade `crates/iroh-rooms` (IR-0301), whose `examples/07_agent_status.rs` is
explicitly the *seed* for an example agent.

This issue ships the **minimal example agent** that turns that seed into a runnable,
adapt-me-as-a-template program demonstrating the **intended integration model**: a third-party Rust
program drives an Iroh Rooms room *through the SDK* — not by shelling out to the `iroh-rooms`
binary — to

1. set up its **own** local identity (no central-service credentials),
2. **join a room by ticket** (the ticket is the sole capability it holds),
3. post one or more signed **`agent.status`** updates, and
4. *optionally* **share one artifact** (a `file.shared` blob) as a demonstration of the "do work,
   publish a result" loop.

The example is a runnable cargo `example` target in the SDK crate, driven by real command-line
arguments (so it "runs from a clean checkout"), plus a co-located `README.md` that explains — for an
integrator — exactly which lines to change to adapt it into their own agent. Correctness is proven by
one new integration test tiered on the repository's established CI-reliability convention: a
deterministic, network-free CI tier (identity persistence + offline authoring/validation) that always
runs, and an `#[ignore]`-gated loopback tier that runs the built example binary against an in-process
admin node and asserts the agent's signed `agent.status` event lands in the room tail — exactly the
issue's Test Plan.

> **Why `risk/medium`.** No new crypto, transport, event type, authorization primitive, CLI command,
> or library API. The risk is entirely in *fidelity and honesty*: an example is a teaching surface, so
> the failure mode is an example that (a) misrepresents the authorization model (e.g. implies the
> agent has privileges it does not), (b) drifts out of sync with the real join/status flow, or (c)
> claims "runs from a clean checkout" without a test that actually runs it. The plan below is shaped to
> foreclose all three: the example is a thin, honest driver over the *same* SDK the CLI uses, its
> authorization posture is spelled out in code and README, and a test executes the built binary.

---

## 2. Background & current repository state

### 2.1 What already exists (landed — this builds on it, adds no runtime code)

- **Agent identity & invite (IR-0206 / #31).** An agent is an *ordinary principal* with its own
  Ed25519 identity+device key — created with the same `identity create` a human uses — that becomes a
  member only through an admin-issued, key-bound invite (`agent invite <ROOM_ID> <AGENT_ID>`, a thin
  wrapper over `room invite --invitee <AGENT_ID> --role agent`) plus its own `room join`. `Role::Agent`
  is least-privileged in the `Agent < Member < Admin` lattice. There is **no** distinct agent principal
  type and **no** implicit room access. (README "Agent identity"; `docs/getting-started.md` Step 3/7.)
- **Agent status (IR-0208 / #33).** `agent.status` is a signed content event carrying a bounded
  `state` (≤64 UTF-8 bytes), optional `message` (≤4096 bytes), optional integer `progress` (0..=100),
  and up to 16 `related_artifact_ids` (the same `file_<hex>` handle codec `file share`/`file fetch`
  use). Posting is **not** role-gated — any active member may post (`gate_active_member`, spike §7) —
  authored by the poster's own key, offline-first / online-best-effort (the same contract as
  `room send`). Built by the pure `build_agent_status(...)` core builder.
- **Rust SDK façade (IR-0301 / #36).** `crates/iroh-rooms` re-exports the shipped surface through
  `identity` / `room` / `events` / `files` / `pipes` (stable, default features) plus an `experimental`
  namespace (`session` / `sync` / `store` / `blob` / `pipe_runtime`) for the online runtime. The exact
  items this example needs already exist and are re-exported:
  - `identity::{SigningKey, IdentityKey, DeviceBinding}` — key generation, `to_seed()`/`from_seed()`,
    `identity_key()`, device-binding creation (`crates/iroh-rooms/src/identity.rs`).
  - `room::{RoomInviteTicket, build_member_joined, Role, RoomId}` — ticket decode + the join builder
    (`crates/iroh-rooms/src/room.rs`).
  - `events::{build_agent_status, validate_wire_bytes, ValidationContext, WireEvent}`
    (`crates/iroh-rooms/src/events.rs`).
  - `files::{build_file_shared, FileShared, HashRef}` (for the optional artifact share).
  - `experimental::session::{Node, NetConfig, NetMode, AllowlistAdmission, JoinBootstrapAdmission,
    TracingAudit, PeerConnState, DEFAULT_TICK}`, `experimental::store::EventStore`,
    `experimental::sync::{SyncEngine, SyncConfig}`, `experimental::blob::{BlobStore, BlobImport}`
    (all behind `--features experimental`).
  - `RoomInviteTicket` public fields the example reads: `room_id`, `invite_id`, `capability_secret`,
    `invitee_key`, `role`, `expires_at`, `inviter_identity`, `discovery: Vec<DeviceKey>`
    (`crates/iroh-rooms-core/src/ticket.rs`).
- **`Node` high-level API** (`crates/iroh-rooms-net/src/node.rs`, re-exported at
  `experimental::session::Node`): `spawn` / `spawn_room` / `publish` / `room_tail` / `snapshot` /
  `heads` / `wait_for_state` / `connect_to` / `fetch_file` / `shutdown`. `spawn_room` is the managed
  session that reconciles admission from the fold and serves held blobs; `spawn` is the lower-level
  form the seed example uses.
- **Existing SDK examples** (`crates/iroh-rooms/examples/`): `01_identity` … `07_agent_status`, plus
  `offline_author_and_validate`. The online examples (`03`–`07`) are `#[cfg(feature = "experimental")]`
  with a `#[cfg(not(...))]` `main()` fallback, use hard-coded placeholder `const`s (e.g.
  `ROOM_ID = "<PASTE…>"`), and are **compile-only in CI** — they cannot be run without hand-editing.
- **`verify.sh`** already compiles + clippy-lints every example under `--all-features --all-targets`
  (so the experimental body is built and linted) and separately builds examples under default features
  (the `not(experimental)` fallback), and runs SDK doctests. The always-green CI test run
  (`cargo test --workspace --all-targets --all-features`) will pick up the new deterministic test tier;
  `#[ignore]`-gated tests do not run there. (`scripts/verify.sh`; the real CI gate — see
  [[verify-sh-is-the-real-ci-gate]].)

### 2.2 What is missing (this issue)

1. **No runnable example agent.** `07_agent_status.rs` is a compile-only fragment with placeholder
   consts; it cannot be executed, and — critically — it generates a *fresh* identity and then asserts
   it equals `ticket.invitee_key`, which cannot hold for a real ticket (an invite is key-bound to a
   *specific* identity). So it does not model the real end-to-end integration a developer must follow:
   **the agent must publish its identity id *first*, be invited by that id, then redeem the resulting
   ticket with the *same* persisted key.**
2. **No example-facing README** explaining how to adapt the example (issue AC4), nor a persisted-
   identity story (the example needs a stable key across the "print id" → "join" steps).
3. **No test that runs an example agent** and verifies a signed status event appears in the tail
   (issue Test Plan).

---

## 3. Goals & non-goals

### 3.1 Goals

- **G1.** A minimal, runnable example agent driven by real CLI args, living in `crates/iroh-rooms`,
  that: generates/persists its own identity, joins a room by ticket, and posts `agent.status`.
- **G2.** Demonstrate the **intended integration model** = the Rust SDK (`iroh-rooms` façade,
  `experimental` tier), not CLI-wrapping. Every room interaction goes through the façade.
- **G3.** The agent requires **no central-service credentials** — it holds only a locally-generated
  keypair and the room capability the ticket granted (AC2/AC3).
- **G4.** A co-located `README.md` that explains the run flow *and* the exact adaptation points (AC4).
- **G5.** An integration test proving the built example runs in a local demo room and its signed
  `agent.status` appears in the room tail (AC1 + Test Plan), tiered so CI stays deterministic.
- **G6 (optional).** An opt-in `--artifact <PATH>` capability that shares one file (`file.shared`) and
  keeps the node online serving it, demonstrating the "publish a result" half of an agent loop.

### 3.2 Non-goals

- **NG1.** No new library surface on `iroh-rooms` — the example is a consumer, not an API. (If a
  genuinely reusable helper is tempting, resist: the SDK is a curated façade, not a utility box. See OQ2.)
- **NG2.** No new CLI command, event type, authorization rule, ticket field, or transport behavior.
- **NG3.** Not a production agent framework: no scheduling, retries beyond the SDK's, config files,
  daemonization, multi-room, or plugin surface. "Minimal" is load-bearing.
- **NG4.** No pipe/preview exposure in the mandatory path (it needs a live local TCP service to be
  meaningful); the README points at `06_live_pipe.rs` as the adaptation for anyone who wants it (the
  issue's "expose a preview" is satisfied as a documented extension; the *implemented* optional
  capability is the artifact share, G6).
- **NG5.** No relay/discovery dependence in the test tier — loopback only, mirroring the repo's other
  online e2e suites (`join_e2e.rs`, `two_peer_e2e.rs`, `full_demo_e2e.rs`).

---

## 4. Design decisions

**D1 — Form factor: a runnable cargo example in the SDK crate, not a new workspace crate.**
Add `crates/iroh-rooms/examples/example_agent/main.rs` (cargo auto-discovers `examples/*/main.rs` as
an example target named `example_agent`) with a co-located `crates/iroh-rooms/examples/example_agent/README.md`.
Rationale: consistent with the existing `01`–`07` examples; reuses the SDK crate's dep graph and its
`experimental` feature and `[dev-dependencies]` (`tokio`, `iroh`, `hex`, `anyhow`, `getrandom`) with
zero new manifests; the SDK *is* the "intended integration model" this issue must demonstrate; and a
new workspace crate would add member/CI ceremony for no teaching gain. A subdirectory example (vs. a
flat `examples/example_agent.rs`) is chosen precisely so the required README (AC4) lives *next to* the
code it documents. (Alternative — a standalone `crates/example-agent` binary crate — is recorded in
§10 / OQ1.)

**D2 — Integration model = SDK library calls, end-to-end, no subprocess shell-outs.** The example
brings up its own `Node` and drives `publish` / `heads` / `snapshot` / `build_*` directly. It does not
invoke the `iroh-rooms` binary. This is what makes it an *integration* example (PRD §19 deliverable 8
builds on deliverable 1, the SDK). It is behind `--features experimental` (the online tier) with the
same `#[cfg(not(feature = "experimental"))]` `main()` fallback the sibling online examples use.

**D3 — Two-phase identity, persisted to a file (the fix for the `07` seed's fatal shortcut).** An
invite ticket is key-bound to a *named* invitee identity (`ticket.invitee_key`), so a fresh random key
cannot redeem a real ticket. The example therefore models the real sequence with a **persisted**
identity across two subcommands:

- `example_agent identity [--identity-file <PATH>]` — generate an identity+device keypair (or reuse an
  existing file), persist it, and print the agent's `identity_id` (hex) for the admin to invite.
- `example_agent join --identity-file <PATH> --ticket <ROOMTKT> [--peer <ENDPOINT_ADDR>]…
  [--status <STATE>] [--message <TEXT>] [--progress <0..100>] [--artifact <PATH>] [--loopback]` —
  load the persisted identity, redeem the ticket (asserting `identity == ticket.invitee_key`),
  post status, optionally share an artifact, then shut down cleanly.

  Persistence format (minimal, documented in the README): a small file containing the two 32-byte
  seeds as lowercase hex (identity seed, device seed), written `0600` on Unix. Uses only
  `SigningKey::from_seed`/`to_seed` and `hex` (already a dev-dep). This deliberately does **not**
  reuse the CLI's `identity.json`/`identity.secret` layout (the SDK does not expose that persistence,
  and reusing it would couple the example to a CLI-internal format — the README notes the CLI's own
  identity is compatible only via `identity show`'s printed id + a fresh SDK identity, i.e. an
  integrator points `agent invite` at whichever identity id the example prints).

**D4 — Capabilities explicit and limited to room authorization (AC3), spelled out in code + README.**
On join, the example seeds an `AllowlistAdmission` from *only* the ticket's `discovery` (the admin's
device key) with `inviter_identity` marked active — it will dial/accept **no one else** until the fold
teaches it otherwise. It joins with the ticket's `role` (expected `agent`, the least-privileged
lattice role) and authors only `member.joined`, `agent.status`, and (optionally) `file.shared`. It
never authors `member.invited`/`member.removed`, never claims admin, and every event it emits is
gated by the same `gate_*` fold checks every peer runs. A prominent code comment and a README section
state the invariant verbatim: *the only capability the agent holds is the room membership the ticket
granted; everything it can do, any active room member can do; remove the agent (`member.removed`) or
let the invite expire and it can do nothing.*

**D5 — Runnable + verifiable, tiered for deterministic CI (AC1 + Test Plan).** One new test file
`crates/iroh-rooms/tests/example_agent_e2e.rs`:
- **CI tier (always runs; deterministic; network-free):** identity file write→read→write round-trip
  (same seeds, same `identity_id`); ticket-parse → offline `build_member_joined` + `build_agent_status`
  → `validate_wire_bytes` accepts (proves the authoring the example does is well-formed without a
  network); and — implicitly via `cargo test --all-targets --all-features` — the example *compiles and
  clippy-passes* under `experimental`. These reconstruct the example's pure steps at the library layer
  so the guarantee is green in CI even though the built binary is exercised only in the gated tier.
- **Online tier (`#[ignore]`-gated; loopback; no relay/discovery):** stand up an in-process admin
  `Node` over `NetMode::Loopback` with join-bootstrap admission (mirroring `join_e2e.rs`); mint an
  `agent`-role `RoomInviteTicket` bound to a *fixed* agent seed; write that seed to a temp identity
  file; run the **built example binary** as a child process
  (`cargo run -q -p iroh-rooms --features experimental --example example_agent -- join --identity-file
  <f> --ticket <t> --peer <admin-loopback-addr> --status running_tests --message "…" --progress 40
  --loopback`); then poll the admin node's `room_tail(..)` until an `agent.status` event **signed by the
  agent's device key** with `state=running_tests` appears, asserting type, signer, and fields — this is
  the issue's Test Plan ("verify signed status event appears in room tail"). Bounded by a timeout so a
  failure is a clear assertion, never a hang.

  The gated tier is documented to run with:
  ```bash
  cargo test -p iroh-rooms --features experimental --test example_agent_e2e -- --ignored --test-threads=1
  ```

**D6 — Discoverability without doc drift.** Add a one-line pointer to the example from the SDK crate
docs (`crates/iroh-rooms/src/lib.rs` "Getting started") and a one-sentence mention in
`docs/getting-started.md` "Using it as a library". Both are additive; `tests/docs_conformance.rs` only
asserts *presence* of specific strings, so an added line cannot break it. Keep these edits minimal —
they are doc-only and non-load-bearing; the mandatory deliverable is the example + README + test.

**D7 — Honesty of the demo.** The example prints the same availability caveats the rest of the product
states (offline-first status; `delivered: N` best-effort; no guaranteed offline delivery). It exits 0
after a successful local store of the status even if zero peers were reachable (matching `room send`),
so "runs from a clean checkout" does not secretly require a live peer to *succeed* — only the *Test
Plan's* "appears in another peer's tail" assertion needs the loopback admin, which the gated tier
supplies.

---

## 5. Detailed implementation steps

> Ordered so each step compiles/tests before the next. All paths under `crates/iroh-rooms/`.

### Step 1 — Scaffold the example target and its no-`experimental` fallback

1. Create `examples/example_agent/main.rs`. Top-level shape mirrors the sibling online examples:
   - A `#[cfg(feature = "experimental")] #[tokio::main] async fn main() -> anyhow::Result<()>` real body.
   - A `#[cfg(not(feature = "experimental"))] fn main()` that prints
     `this example requires \`--features experimental\`` (byte-for-byte the sibling wording).
2. Confirm cargo discovers it: `cargo build -p iroh-rooms --examples --features experimental` lists/builds
   `example_agent`. (No `Cargo.toml` `[[example]]` entry is required — auto-discovery handles
   `examples/*/main.rs`. Add an explicit `[[example]] name = "example_agent" path = "…"` only if
   auto-discovery misbehaves; see OQ3.)

### Step 2 — Argument parsing (dependency-light)

3. Parse `std::env::args()` by hand (no `clap` — keep the example minimal and dep-free; the CLI crate
   owns `clap`). Support exactly two subcommands and the flags in D3. Unknown/missing args print a
   short usage string to stderr and exit non-zero. Keep the parser ~40 lines; it is not the teaching
   focus — the SDK calls are.
4. Flag defaults: `--identity-file` defaults to `./example-agent.identity`; `--status` defaults to
   `running_tests`; `--progress` optional; `--loopback` toggles `NetMode::Loopback` +
   relay-disabled (for CI/LAN determinism), else `NetConfig::default()`.

### Step 3 — Identity setup (`identity` subcommand + shared load/save helpers)

5. `fn save_identity(path, &SigningKey identity, &SigningKey device) -> Result<()>`: write the two
   `to_seed()` values as two lines of lowercase `hex` to `path`; set Unix perms `0600`
   (`std::os::unix::fs::PermissionsExt`, `#[cfg(unix)]`). Refuse to overwrite an existing file unless
   `--force` (so a second `identity` run does not silently rotate the key the admin already invited).
6. `fn load_identity(path) -> Result<(SigningKey, SigningKey)>`: read + `hex`-decode the two seeds,
   rebuild via `SigningKey::from_seed`. Actionable error if the file is missing ("run `example_agent
   identity` first") or malformed.
7. `identity` subcommand: if file absent (or `--force`), generate via `SigningKey::generate()` and
   save; then print `identity_id: <hex>` and a next-step hint
   (`next: have the room admin run \`iroh-rooms agent invite <ROOM_ID> <THIS_ID>\``). Round-tripping an
   existing file prints the same id (idempotent), which the CI-tier test asserts.

### Step 4 — Join by ticket (the core of the integration model)

8. `join` subcommand body (adapted from `03_invite_and_join.rs`, corrected for real tickets):
   1. `let ticket: RoomInviteTicket = ticket_arg.trim().parse()?;` — a decode failure is the honest
      ticket error (the README maps it to the CLI's `ticket_*` codes for readers).
   2. `let (identity, device) = load_identity(path)?;`
   3. `anyhow::ensure!(identity.identity_key() == ticket.invitee_key, "this identity was not the one
      invited — have the admin \`agent invite\` the id printed by \`example_agent identity\`");` — the
      key AC3/UX correctness the seed example got wrong.
   4. Seed admission (D4): `AllowlistAdmission::new()`, for each `dev` in `ticket.discovery`
      `.bind_device(EndpointId::from_bytes(dev.as_bytes())?, ticket.inviter_identity)`, then
      `.set_active(ticket.inviter_identity)`.
   5. `EventStore::open_in_memory()?` → `SyncEngine::open(store, ticket.room_id, SyncConfig::default())`
      → `Node::spawn(SecretKey::from_bytes(&device.to_seed()), Arc::new(admission), Arc::new(TracingAudit),
      engine, netcfg, DEFAULT_TICK).await?`.
   6. `node.connect_to(EndpointAddr::new(admin_id))` for the ticket's discovery hint and each `--peer`;
      `node.wait_for_state(admin_id, PeerConnState::Connected, Duration::from_secs(10)).await?`.
   7. Poll for the membership sub-DAG: loop up to a bounded deadline calling `node.snapshot()` until the
      agent's own identity resolves to `Invited`/`Active` (the never-windowed membership pull always
      converges once connected). This replaces `03`'s bare `sleep(1s)` — a real agent must *confirm* it
      was invited before authoring the join. Time out with an actionable error
      ("could not bootstrap membership — is the admin running `room tail <ROOM_ID> --accept-joins`?").
   8. Build + self-validate + publish the join:
      `let binding = DeviceBinding::create(&ticket.room_id, &identity, device.device_key());`
      `let heads = node.heads().await?;`
      `let wire = build_member_joined(&identity, &device, &ticket.room_id, &ticket.invite_id,
      &ticket.capability_secret, &ticket.role, binding, /*display_name*/ Some("example-agent"), &heads,
      now_ms());`
      `validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(ticket.room_id))?;`
      `node.publish(wire.to_bytes()).await?;`
   9. Confirm the local `Active` transition (poll `node.snapshot()` again, bounded) so the agent knows
      it is a member before it starts posting — then print `joined: <room_id> role=<role>`.

### Step 5 — Post `agent.status`

9. After a confirmed join, run a small illustrative status loop (kept from the `07` seed but
   arg-driven): post the `--status`/`--message`/`--progress` the user passed once, and — to show the
   "progress" idiom — optionally a short scripted sequence (e.g. `(running, 40) → (done, 100)`) when no
   explicit `--progress` is given. Each iteration:
   `let heads = node.heads().await?;`
   `let wire = build_agent_status(&identity, &device, &ticket.room_id, status, message, &artifact_ids,
   progress, &heads, now_ms());`
   `node.publish(wire.to_bytes()).await?;`
   Print the same `stored: yes` / `delivered: <n>` honesty lines the CLI prints (D7). `artifact_ids` is
   empty unless Step 6 produced one.

### Step 6 — (Optional, G6) Share one artifact

10. If `--artifact <PATH>` is given, *before* the status loop:
    1. Canonicalize the path to absolute (iroh-blobs `add_path` rejects relative paths — see
       [[blob-add-path-requires-absolute]]).
    2. Open a `BlobStore` at a temp/`--data-dir` blob directory, `import`/`BlobImport` the file, get the
       BLAKE3-256 `HashRef`, independently recompute + assert it matches (the same integrity the CLI
       does).
    3. `build_file_shared(&identity, &device, &ticket.room_id, file_id, name, mime, size, hash, …,
       &heads, now_ms())` → self-validate → `node.publish(...)`.
    4. Add the `file_id` to `artifact_ids` so the subsequent `agent.status` references it.
    5. Keep the node online long enough to serve at least one fetch (a fixed grace, documented as the
       "provider stays online" caveat), then close the `BlobStore` (`shutdown`) before exit — the FsStore
       holds an exclusive lock (see [[fsstore-exclusive-lock-needs-shutdown]]). Consider using
       `Node::spawn_room` instead of `spawn` for this path, since it serves held blobs and reconciles the
       ACL from the fold; gate the choice on whether `--artifact` was supplied to keep the default path
       minimal.
    > Mark Step 6 lower priority than Steps 1–5; if descoped it becomes the first README "adaptation"
    > and the issue's "optionally … if MVP APIs are ready" clause is satisfied by the mandatory status
    > path alone.

### Step 7 — Clean shutdown & `now_ms`

11. On all success/early-return paths: `node.shutdown().await?` after a short flush grace
    (`sleep(500ms)`, as the sibling examples do), so the last published frame reaches connected peers.
    Reuse the sibling `now_ms()` helper verbatim (wall-clock ms since epoch; the builders take the clock
    as an injected input, so this stays out of any trust decision).

### Step 8 — README (AC4)

12. Write `examples/example_agent/README.md` covering:
    - **What it is / integration model:** a minimal Rust agent driving a room through the `iroh-rooms`
      SDK (`experimental` tier), the runnable evolution of `07_agent_status.rs`.
    - **Run it from a clean checkout** — the full three-terminal local demo, reusing
      `docs/getting-started.md`'s alias/`IROH_ROOMS_HOME` conventions: (a) admin (human, real
      `iroh-rooms` binary) `identity create` + `room create` + `room tail --accept-joins`; (b) this
      example `identity` to print its id; (c) admin `agent invite <ROOM_ID> <PRINTED_ID>` → ticket; (d)
      this example `join --ticket <T> --peer <ADMIN_ADDR> --status …`; (e) admin `room tail --offline`
      shows the signed `agent.status`. This makes the example interoperate with the shipped binary,
      which is the most convincing "clean checkout" proof.
    - **Adaptation guide (the AC4 core):** an explicit, line-referenced list of what to change —
      the status `state`/`message`/`progress` you emit; the work you do between statuses; wiring your
      own identity persistence (or an existing key); adding `--artifact` result-publishing; and the
      pointer to `06_live_pipe.rs` for exposing a live preview instead.
    - **Authorization posture (AC3), stated plainly:** the D4 invariant — capabilities limited to room
      membership; no central credentials; remove/expire ⇒ zero capability.
    - **Availability honesty:** the PRD §14 bullets (no cloud inbox, no guaranteed offline delivery,
      provider-stays-online for `--artifact`).

### Step 9 — Test (`tests/example_agent_e2e.rs`), per D5

13. CI tier (deterministic): identity round-trip; offline `member.joined` + `agent.status` authoring
    validates. These do **not** require `--features experimental` for the pure-authoring asserts (they
    use only stable `room`/`events`/`files` items); put the identity-file helpers behind a tiny shared
    test module or duplicate the trivial hex round-trip inline.
14. Online tier (`#[ignore]`): the loopback admin-node + child-process run + tail assertion (D5). Use
    the same in-process `Node` + `JoinBootstrapAdmission` setup as `crates/iroh-rooms-net/tests/join_e2e.rs`;
    launch the built example with `std::process::Command::new(env!("CARGO"))` running
    `run -q -p iroh-rooms --features experimental --example example_agent -- join …` (or `escargot` if
    the team prefers a prebuilt-binary path — OQ4). Bound every wait with a timeout.

### Step 10 — Discoverability + gate (per D6)

15. Add the SDK-doc + getting-started one-liners (D6). Run `scripts/verify.sh`: it already
    fmt/clippy/tests `--all-features --all-targets` (compiles+lints the experimental example body and
    runs the CI-tier test), builds examples under default features (the `not(experimental)` fallback),
    and runs SDK doctests. **No `verify.sh` change is required** for compile/lint coverage. Run the
    gated tier locally with the command in D5. Update the README's "Repository Layout"/examples mention
    only if the team wants the new example listed there (optional).

---

## 6. File-by-file change list

| Path | New/edit | Change |
|---|---|---|
| `crates/iroh-rooms/examples/example_agent/main.rs` | **new** | The runnable example: arg parse, identity save/load, join-by-ticket, `agent.status` loop, optional `--artifact` share, clean shutdown. Gated `#[cfg(feature = "experimental")]` with the standard non-experimental `main()` fallback. |
| `crates/iroh-rooms/examples/example_agent/README.md` | **new** | Adaptation guide + clean-checkout run flow + authorization posture + availability honesty (AC4). |
| `crates/iroh-rooms/tests/example_agent_e2e.rs` | **new** | CI-tier (identity round-trip, offline authoring validates) + `#[ignore]` loopback tier (child-process run → admin tail assertion; the issue Test Plan). |
| `crates/iroh-rooms/src/lib.rs` | edit (doc-only) | One-line "Getting started" pointer to the example agent. Additive doc comment; no code change. |
| `docs/getting-started.md` | edit (doc-only) | One sentence under "Using it as a library" pointing at the example agent. Additive; safe for `docs_conformance.rs`. |
| `crates/iroh-rooms/Cargo.toml` | edit **only if** auto-discovery fails (OQ3) | Explicit `[[example]] name = "example_agent"`. Expected: **not needed.** |
| `crates/iroh-rooms/CHANGELOG.md` | edit (optional) | Note the added example under the developer-preview changelog. |

**No production/runtime source file changes.** `iroh-rooms-core`, `iroh-rooms-net`, and
`iroh-rooms-cli` are untouched.

---

## 7. Test plan

### 7.1 Deterministic CI tier (always runs via `cargo test --workspace --all-targets --all-features`)

- **Identity persistence round-trip.** Generate → save → load → re-derive `identity_id`; assert the id
  is stable across the round-trip and that a second `save` without `--force` refuses to overwrite.
- **Offline authoring is well-formed.** From a synthetic agent-role ticket (minted in-test with the
  landed `build_member_invited` + ticket codec, as `join_e2e.rs`/`agent_invite_flow.rs` do), build
  `member.joined` and `agent.status` with the example's exact argument shape and assert
  `validate_wire_bytes` accepts both under `ValidationContext::for_room(room_id)`, and that the
  `agent.status` is signed under the agent's device key with the expected `state`/`progress`.
- **Compile + lint.** `cargo clippy --all-features --all-targets -D warnings` and the `--all-targets`
  test build compile the experimental example body; the default-features `cargo build -p iroh-rooms
  --examples` compiles the `not(experimental)` fallback (both already in `verify.sh`).

### 7.2 Gated loopback tier (`#[ignore]`; the issue Test Plan)

```bash
cargo test -p iroh-rooms --features experimental --test example_agent_e2e -- --ignored --test-threads=1
```

- Stand up an in-process admin `Node` (loopback, relay-disabled, join-bootstrap admission).
- Mint an `agent`-role ticket bound to a fixed agent seed; write that seed to a temp identity file.
- Run the built `example_agent join …` as a child process against the admin's loopback address.
- Poll the admin's `room_tail` until an `agent.status` **signed by the agent's device key** with the
  passed `state` appears; assert type, signer, `state`, `progress`. Fail on timeout (never hang).
- **Negative check:** run `join` with a fresh (uninvited) identity file against the same ticket and
  assert the identity-binding `ensure!` fails fast with the actionable message and a non-zero exit,
  before any network IO (AC3 in the honest direction — an agent cannot redeem a capability not bound to
  it).

### 7.3 Manual acceptance (README-driven, the "clean checkout" proof)

Follow `examples/example_agent/README.md` end-to-end against a freshly built `iroh-rooms` binary as the
admin/human peer (three terminals, per `docs/getting-started.md`), and confirm the agent's signed
`agent.status` renders in the admin's `iroh-rooms room tail <ROOM_ID> --offline`. This is the
human-facing form of the Test Plan and doubles as README reconciliation.

---

## 8. Acceptance criteria → evidence

| Issue AC | How this spec satisfies it | Evidence |
|---|---|---|
| **Example runs from a clean checkout.** | Arg-driven, runnable example (`cargo run -p iroh-rooms --features experimental --example example_agent -- …`); README gives the exact clean-checkout run flow interoperating with the shipped binary; a gated test runs the built binary. | §5 Steps 1–7; §7.2; §7.3; README (Step 8) |
| **Agent does not require central service credentials.** | Identity is a locally-generated Ed25519 keypair persisted to a local file; the only other input is the room ticket. No account, no server login, no API key anywhere. | §5 Step 3 (D3); README authorization section |
| **Agent capabilities are explicit and limited to room authorization.** | Admission seeded solely from the ticket's discovery hint; joins as least-privileged `Role::Agent`; authors only member.joined/agent.status/(optional) file.shared; every event re-gated by the fold; invariant stated in code + README; negative test proves an unbound identity cannot redeem the capability. | §5 Steps 4/8 (D4); §7.2 negative check |
| **README explains how to adapt the example.** | Co-located README with an explicit, line-referenced adaptation guide (status content, work loop, identity wiring, artifact/preview extension). | §5 Step 8; §6 |
| **Test Plan: run in a local demo room; verify signed status event in room tail.** | Gated loopback tier runs the built example against an in-process admin node and asserts the signed `agent.status` appears in `room_tail`; manual README flow does the same against the binary. | §5 Step 9; §7.2; §7.3 |

---

## 9. Risks & mitigations

- **R1 — Example drifts out of sync with the real join/status flow.** The join orchestration
  (`join.rs`) and status flow (`message.rs`) are the production truth; a hand-trimmed example can rot.
  *Mitigation:* keep the example a thin driver over the *same* SDK builders/`Node` the CLI uses (no
  re-implemented logic); the gated test executes the built binary, so a breaking SDK change fails the
  test, not just review. Cross-reference the production sources in the module doc header (as `03` does).
- **R2 — The `07`-seed identity shortcut (fresh key == ticket key) reappears.** *Mitigation:* D3's
  two-phase persisted identity + the Step 4.3 `ensure!` + the §7.2 negative test make the correct model
  the only one that passes.
- **R3 — "Runs from a clean checkout" is only compile-checked, not run.** *Mitigation:* the gated tier
  actually launches the binary; the README manual flow is reconciled against the shipped binary (like
  the getting-started guide's reconciliation discipline).
- **R4 — Flaky online test (loopback QUIC, child process).** *Mitigation:* loopback + relay-disabled +
  `--test-threads=1` + bounded timeouts, exactly the pattern `join_e2e.rs`/`two_peer_e2e.rs` already use
  reliably; keep it `#[ignore]` so CI green never depends on it.
- **R5 — Blob-store footguns in the optional `--artifact` path.** Relative-path rejection and the
  exclusive FsStore lock are known sharp edges. *Mitigation:* canonicalize before import
  ([[blob-add-path-requires-absolute]]); `BlobStore::close`/`shutdown` before exit
  ([[fsstore-exclusive-lock-needs-shutdown]]); mark the whole step optional/descopable.
- **R6 — Scope creep into a "framework."** *Mitigation:* NG3 + a hard line: no config files, no
  scheduler, hand-rolled arg parsing, ≤~250 lines. If a helper feels reusable, it belongs in a future
  SDK issue, not here (OQ2).
- **R7 — Doc-conformance / README reconciliation.** Adding lines to `docs/getting-started.md` is safe
  (presence-only asserts) but the README's transcribed CLI output must match the binary. *Mitigation:*
  keep README command output blocks minimal and copy them from a real run, per the repo's
  "binary is the source of truth" rule.

---

## 10. Rollout / rollback

- **Rollout:** purely additive — a new example target, a README, a test file, and two doc one-liners.
  Nothing ships in the `iroh-rooms` library surface or any binary; no migration, no feature flag beyond
  the existing `experimental`. Landing it changes no runtime behavior.
- **Rollback:** delete the two new files (+ the two doc one-liners); zero blast radius. Because the
  example lives behind `--features experimental` and the online test is `#[ignore]`, reverting cannot
  affect the default build, the stable SDK surface, or CI green.

---

## 11. Open questions

- **OQ1 — Example vs. standalone crate.** This spec chooses a cargo example in `crates/iroh-rooms`
  (D1). If the team wants the example agent to be *more* prominent / independently versionable, a
  standalone `crates/example-agent` binary crate is the alternative (its own README-at-root, its own
  `[[bin]]`, discoverable in the workspace layout table) at the cost of a new workspace member and dep
  wiring. Recommendation: start as an example; promote to a crate only if a second consumer appears.
- **OQ2 — Any reusable helper?** The identity persistence + admission-from-ticket seeding are mildly
  reusable. Deliberately kept inside the example (NG1). If multiple future examples/integrations want
  them, that is a *separate* SDK issue to add a small `iroh_rooms::agent`-style convenience layer with
  its own stability commitment — not smuggled in via an example.
- **OQ3 — Example auto-discovery.** Confirm cargo discovers `examples/example_agent/main.rs` as
  `--example example_agent` without an explicit `[[example]]` stanza (expected: yes). If not, add the
  stanza (§6).
- **OQ4 — Child-process launch mechanism in the gated test.** `Command::new(env!("CARGO")) run
  --example …` (simplest, rebuilds on demand) vs. `escargot` (prebuild once, exec the artifact,
  faster/less noisy). Recommend the plain `cargo run --example` form unless build noise in the test log
  is a problem.
- **OQ5 — Should the default (no-flag) `join` post a scripted status sequence or just one status?** The
  spec suggests a short `(running,40) → (done,100)` demo when `--progress` is omitted, to show the
  progress idiom; the team may prefer a single status for maximum minimality. Low stakes; README
  documents whichever is chosen.

## 12. Assumptions

- **A1.** The intended "integration model" is the **Rust SDK** (façade `iroh-rooms`, `experimental`
  tier), not a shell script wrapping the binary — grounded in PRD §19 (deliverable 8 "example agent"
  follows deliverable 1 "Rust SDK") and the `07_agent_status.rs` seed header. If the intent were a
  language-agnostic CLI-driven agent, the form would shift to a shell/Python script driving
  `iroh-rooms` subcommands; the join-by-ticket → `agent status` → tail-verify flow is identical either
  way, but this spec commits to the SDK form.
- **A2.** `SigningKey` exposes `generate()`, `to_seed()`, and `from_seed(&[u8; 32])` (the seed round-trip
  the examples already rely on via `to_seed()`). **Verified** at spec time:
  `crates/iroh-rooms-core/src/event/keys.rs:231` defines `pub fn from_seed(seed: &[u8; PUBLIC_KEY_LEN])`
  (`PUBLIC_KEY_LEN == 32`) and `:263` `pub fn to_seed(&self) -> Zeroizing<[u8; PUBLIC_KEY_LEN]>`, both
  re-exported through `iroh_rooms::identity::SigningKey`. The persistence helper therefore hex-encodes
  `&*seed` (deref the `Zeroizing` guard) on save and passes a `&[u8; 32]` to `from_seed` on load; the
  `Zeroizing` wrapper zeroes the in-memory seed on drop, which the helper must not defeat by copying the
  bytes into a long-lived `String` beyond the hex line it writes.
- **A3.** An in-process admin `Node` over loopback with `JoinBootstrapAdmission` can admit and converge
  a child-process agent's join purely over loopback QUIC (no relay/discovery) — the same capability
  `join_e2e.rs` exercises at the Node layer; here one side is a separate OS process, which QUIC handles
  identically.
- **A4.** `RoomInviteTicket.discovery` carries the admin's device key(s) sufficient to dial the admin on
  loopback when combined with the test-supplied `--peer <addr>` (the ticket alone need not carry a
  routable address in the loopback/CI setting — `--peer` provides it, exactly as `room join --peer`
  does).
- **A5.** Editing `crates/iroh-rooms/src/lib.rs` doc comments and `docs/getting-started.md` prose counts
  as documentation, not "production code," and is within scope for this docs+feature issue (labels
  include `type/docs`). If the executing agent treats any `.rs` edit as out of bounds, the `lib.rs`
  one-liner is droppable without affecting any AC.
