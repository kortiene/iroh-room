# Spec: Define the Initial Rust SDK Surface (IR-0301)

| | |
|---|---|
| **Issue** | #36 — `[IR-0301] Define initial Rust SDK surface` |
| **Parent** | #4 |
| **Labels** | `type/feature` `area/protocol` `area/dx` `priority/p1` `risk/medium` |
| **Dependencies** | #34 (IR-0209, full-demo integration test — **landed**; proves the core flow the SDK codifies) |
| **Traceability** | PRD `PRD.v0.3.md` §19 Phase 2 deliverable 1 ("Rust SDK"), §17.2 Developer Experience Metrics; the shipped public surfaces of `crates/iroh-rooms-core` and `crates/iroh-rooms-net`; `docs/getting-started.md` (the demo the examples mirror) |
| **Owning crates** | **New** `crates/iroh-rooms` (the façade/umbrella library). No behavior change to `iroh-rooms-core` / `iroh-rooms-net` (additive doc attributes only); optional import migration in `crates/iroh-rooms-cli`. |

> **Status:** planned — this document is the build plan. It defines an API *surface* and a
> migration/proof plan; it does **not** add product features. The compiled crates are the source of
> truth for what already exists (§2).

---

## 1. Summary

The CLI MVP has proven the core flow end-to-end (identity → room → invite/join → message → file →
live pipe → agent status), and `full_demo_e2e.rs` (#34) locks it. Phase 2 ("Developer Preview")
now wants that same flow reachable **as a library**, so a third-party Rust program (an example
agent, an integration, a future desktop shell) can drive a room without shelling out to the
`iroh-rooms` binary.

This issue **defines and documents the initial supported Rust SDK boundary** — it does not build a
new subsystem. Concretely:

1. Introduce a **façade crate `iroh-rooms`** (package `iroh-rooms`, distinct from the binary crate
   `iroh-rooms-cli`) that re-exports a **curated, documented, stability-tiered** surface drawn from
   the already-shipped `iroh-rooms-core` and `iroh-rooms-net`, organized into the five domain
   modules the issue names: `identity`, `room`, `events`, `files`, `pipes`.
2. Split that surface into two tiers: a **stable** default surface (the deterministic,
   conformance-tested, byte-stable protocol layer) and an **experimental** surface behind the
   `experimental` cargo feature (the online runtime: transport, sync engine, local store, blob
   serve/fetch, live-pipe forwarding).
3. **Document it with compilable examples** — cargo `examples/` mirroring the getting-started demo,
   plus doctests on the module facades.
4. **Prove the CLI can consume the same public surface** — audit that the façade re-exports every
   `core`/`net` symbol the CLI already depends on, and migrate at least the offline authoring path
   to import through the façade (conformance that the boundary is real and sufficient).

The organizing principle — **stable = offline/deterministic protocol, experimental = online
runtime** — is what makes the three shape-related acceptance criteria fall out (§4 D3): experimental
APIs are clearly gated (AC3), and because the online runtime (where the Phase 5 availability layer,
multi-device, and the future call plane will land) is explicitly *experimental and narrowly shaped*,
the **stable** API shape implies no post-MVP capability (AC4).

> **Why `risk/medium`.** No new crypto, transport, event type, or authorization primitive — this is
> re-exports, feature gates, doc attributes, examples, and (optionally) CLI import rewiring. The risk
> is entirely in *commitment*: a public surface is a promise. Picking the tiers wrong (exposing a
> churny type as stable, or hiding one the CLI genuinely needs) is the failure mode, so the plan is
> conservative — stable ⊆ the conformance-tested byte-stable layer — and it uses the CLI as the
> forcing function that the surface is neither too small nor too large.

---

## 2. Background & current repository state

### 2.1 What already exists (landed — this builds on it)

The full MVP protocol + runtime is shipped across three crates. Their **public** surfaces (the raw
material the façade curates) are:

- **`iroh-rooms-core`** (`crates/iroh-rooms-core/src/lib.rs`) — publishable (no `publish = false`).
  Modules: `event` (always), `ticket` (always), `membership` (always), `store` (feature `store`),
  `sync` (feature `sync`, which enables `store`). Notable public re-exports:
  - `event`: `WireEvent`, `SignedEvent`, `EventId`/`RoomId`/`HashRef`, `Content`, `EventType`,
    `capability_hash`, `DeviceBinding`, key types `SigningKey`/`IdentityKey`/`DeviceKey`/`Signature`,
    `validate_wire_bytes`, `validate_with_membership`, `ValidatedEvent`, `ValidationContext`,
    `RejectReason`, `Flag`, `MembershipOracle`, and the pure builders `build_room_created`,
    `build_member_invited`, `build_member_joined`, `build_member_left`, `build_member_removed`,
    `build_message_text`, `build_file_shared`, `build_agent_status`, `build_pipe_opened`,
    `build_pipe_closed`. Submodules `event::constants` (the `MAX_*` bounds), `event::content`
    (content structs incl. `FileShared`, `AgentStatus`, `PipeOpened`), `event::cbor` (the strict
    codec), `event::ids`, `event::keys`, `event::signed` are all `pub`.
  - `ticket`: `RoomInviteTicket`, `TicketError`.
  - `membership`: `MembershipSnapshot`, `Member`, `Role`, `Status`, `RoomMembership`, `Ingest`,
    `AncestorView`, and the access predicates `blob_serve_allowed`, `pipe_connect_allowed`,
    `BlobDecision`, `PipeDecision`, `DenyReason`.
  - `store` (feature): `EventStore`, `StoredEvent`, `StoreError`, `InsertOutcome`, `InsertStats`,
    `ParkedRow`, `SyncStateRow`, `TrustRow`.
  - `sync` (feature): `SyncEngine`, `SyncConfig`, `SyncMessage`, `SyncTransport`, `Completeness`,
    `Severity`, `SyncCounters`, `SyncDigest`, `SyncError`, `TrustDecision`, `Outgoing`, `PeerId`,
    `Window`, `WireBytes`, `MessageError`, and `sync::sim` (the deterministic test harness).
- **`iroh-rooms-net`** (`crates/iroh-rooms-net/src/lib.rs`) — **`publish = false`** today (MSRV note:
  it deliberately does not inherit the workspace `rust-version = 1.80`; the iroh 1.0 stack forces
  rustc ≥ 1.85). Public: `Node` + `BlobServeConfig` + `DEFAULT_TICK` (the high-level runtime),
  `NetTransport`/`NetConfig`/`NetMode`/`Inbound`/`Shared`, admission types (`Admission`,
  `AdmissionDecision`, `AdmissionView`, `AllowlistAdmission`, `JoinBootstrapAdmission`,
  `SnapshotAdmission`, `RejectCause`), `PeerManager`, connection state (`PeerConnState`, `ConnEvent`,
  `OfflineReason`, `PeerEntry`, `PeerTable`), blob runtime (`BlobStore`, `BlobAclView`, `BlobError`,
  `BlobImport`, `FetchOutcome`), pipe runtime (`PipeForwarder`, `PipeRegistry`, `PipeOutcome`,
  `PipeError`, `PipeDenyCause`, `PipeAuditSink`, `TracingPipeAudit`, `new_pipe_id`, `PIPE_ALPN`),
  audit (`AuditSink`, `BlobDenyCause`, `TracingAudit`), ALPN constants, and `EventProtocolHandler`.
  `Node`'s method set (`node.rs`) is the de-facto high-level API: `spawn` / `spawn_room` /
  `spawn_with_pipe_audit`, `publish`, `room_tail`, `snapshot`, `heads`, `fetch_file`, `pipe_expose` /
  `pipe_connect` / `pipe_close`, `conn_events`, `wait_for_state`, `shutdown`, …
- **`iroh-rooms-cli`** (`crates/iroh-rooms-cli`, binary `iroh-rooms`) — **the current
  reference consumer**. It imports directly from `core` and `net` today. An audit of its
  `use iroh_rooms_{core,net}::` lines (all of `src/*.rs`) is the ground-truth coverage list for the
  façade (§5.4). Representative dependence:
  - Stable-tier candidates it uses: `event::{build_room_created, build_member_invited,
    build_member_joined, build_message_text, build_agent_status, build_file_shared, build_pipe_opened,
    capability_hash}`, `event::content::{Content, EventType, FileShared}`, `event::ids::{EventId,
    RoomId, HashRef}`, `event::keys::{SigningKey, IdentityKey, DeviceKey}`, `event::signed::SignedEvent`,
    `event::binding::DeviceBinding`, `event::validate::{validate_wire_bytes, ValidationContext}`,
    `event::constants::{MAX_MESSAGE_BODY_BYTES, MAX_PREV_EVENTS, MAX_SHARED_FILE_BYTES, SHORT_ID_LEN,
    …}`, `event::RejectReason`, `membership::{MembershipSnapshot, Role, Status, RoomMembership,
    Ingest}`, `ticket::{RoomInviteTicket, TicketError}`.
  - Experimental-tier candidates it uses: `store::{EventStore, StoredEvent}`, `sync::{SyncEngine,
    SyncConfig}`, and from net `Node`, `NetConfig`, `DEFAULT_TICK`, `AllowlistAdmission`,
    `AuditSink`, `BlobDenyCause`, `RejectCause`, `OfflineReason`, `PipeAuditSink`, `PipeDenyCause`,
    `pipe::is_loopback_target`, and the blob import/fetch helpers.
- **No façade crate, no `examples/` directory, and no library-level SDK docs exist today.**
  `docs/getting-started.md` documents the *CLI* flow; there is no compilable library example.

### 2.2 What is missing (this issue)

1. A new `crates/iroh-rooms` façade crate: `Cargo.toml`, `src/lib.rs` with the stability policy and
   `#![deny(missing_docs)]`, and the five domain modules + an `experimental` module (§5).
2. Feature wiring: `default = []` (stable-only), `experimental` (enables the online runtime + its
   `core`/`net` features), and a convenience `full` feature.
3. Curated `pub use` re-exports across the two tiers (§5.2–§5.3), each with a `//!`/`///` doc line
   and, on experimental items, an explicit "Experimental" marker (§4 D4).
4. `crates/iroh-rooms/examples/*.rs` mirroring the demo, compiled in CI (§6, §11).
5. Doc examples on the module facades (`cargo test --doc`).
6. A workspace-membership entry + `scripts/verify.sh` coverage (the new crate must build under
   `--all-features` and its examples must compile).
7. An audited coverage table proving the façade re-exports everything the CLI needs, and a
   conformance proof that the CLI compiles against the façade for at least its offline authoring path
   (§5.4, §11 L4).

### 2.3 Non-negotiable inputs from the PRD/roadmap

- PRD §19 Phase 2 lists "Rust SDK" as deliverable 1, alongside protocol docs (#), test vectors
  (already landed, #7/IR-0003), better error handling, and an **example agent** + **example
  dev-preview workflow** (deliverables 8–9) — the examples this SDK's `examples/` seed.
- PRD §7.3 Out of Scope and §9.4 (call plane is "future"), §13.4/§13.5 (multi-device, richer identity
  are roadmap, not MVP), and §19 Phase 5 (the availability layer) are the capabilities the surface
  must **not** imply (AC4).

---

## 3. Scope & non-goals

**In scope**
- A new façade crate re-exporting a curated `identity` / `room` / `events` / `files` / `pipes`
  surface, tiered stable vs experimental (§5).
- Crate-level stability policy + per-item docs + compilable examples + doctests (§4, §6, §11).
- An audit + conformance proof that the CLI's dependencies are all reachable through the façade, and
  a migration of at least the CLI offline authoring path to consume it (§5.4).
- Documenting `iroh-rooms-core` / `iroh-rooms-net` as *implementation crates* with a stability
  disclaimer pointing consumers at the façade.

**Non-goals**
- **No new runtime, event type, transport, or authorization logic.** Zero behavior change to `core`
  / `net`; the only edits there are additive doc attributes and (if chosen) flipping `net`'s
  `publish` flag (§15 OQ3).
- **No visibility narrowing of `core`/`net`.** Their existing `pub` items stay `pub` (tightening to
  `pub(crate)`/sealed is a deliberate, riskier follow-up once the façade proves sufficient — §13 R1,
  §15 OQ4). The initial boundary is established by *documentation, the advertised entry point, and
  feature-gating the online tier*, not by making internals unreachable.
- **No crates.io publication.** Phase 2 is a *developer preview*; git/path dependency is the delivery
  vehicle. The façade stays `publish = false` initially; crates.io release is a follow-up (§15 OQ3).
- **No post-MVP surface**: no multi-device / device-management API, no call-plane types, no
  availability-layer (always-on node / archive peer / pinning) API, no `task.*` / `agent.output`
  event types. These are explicitly excluded from *both* tiers (AC4).
- **No async trait / plugin extensibility framework.** The initial SDK is concrete re-exports, not an
  abstraction layer for alternate transports/stores (the `SyncTransport` trait remains experimental).
- **No stability guarantee on the experimental tier** — it may change every minor release.

---

## 4. Design decisions

**D1 — A façade crate, not just doc-annotating the existing surface.** Two approaches were weighed:
*(A)* leave `core`/`net` as-is and only add docs + "experimental" markers to their existing public
items; *(B)* add a curated umbrella crate that re-exports a shaped subset. **Choose B.** The issue
says "Define the initial Rust SDK *surface*" and enumerates domain groupings (`identity`, `room`,
`events`, `files`, `pipes`) that map to façade modules — approach A cannot *shape* the surface (every
`core`/`net` `pub` item stays equally prominent, so AC4 "no post-MVP capabilities implied" is
unachievable when e.g. the raw `SyncEngine`/admission/transport internals sit at the same level as
`build_message_text`). A façade gives one import (`iroh-rooms`), one place to hang the stability
policy and feature gate, and one target for the examples and the CLI conformance proof. (A is
retained as the fallback if a maintainer rejects a new crate — §15 OQ1.)

**D2 — Crate name `iroh-rooms` (library) vs `iroh-rooms-cli` (binary).** The binary *package* is
`iroh-rooms-cli` producing the `iroh-rooms` *binary*; a library *package* named `iroh-rooms` does not
collide (package names are distinct). `iroh-rooms` is the natural "one crate to depend on" name.
Alternative `iroh-rooms-sdk` is noted (§15 OQ2) but `iroh-rooms` is recommended.

**D3 — Tier by determinism: stable = offline protocol, experimental = online runtime.** The stable
surface is exactly the layer that is (a) pure/deterministic, (b) conformance-tested with byte-exact
golden vectors (#6/#7), and (c) byte-stable by design: event authoring (the `build_*` functions),
the `WireEvent`/`SignedEvent` model, `validate_wire_bytes`, the membership fold + access predicates,
and the ticket codec. The experimental surface is everything with IO, a network, a clock, or a
schema that can still move: the `Node`/transport/admission runtime, the `SyncEngine`, the local
`EventStore` (schema is already at v2), blob serve/fetch, and live-pipe forwarding. This split is not
arbitrary — it aligns the *stability* promise with the layer that actually carries the open
**Gate-A** risk and the future **Phase 5 availability layer** churn, so committing to the stable tier
is genuinely safe. It also makes AC4 automatic: multi-device, calls, and availability all live in (or
below) the online runtime, which is experimental and narrowly shaped, so the *stable* shape implies
none of them.

**D4 — "Clearly marked experimental" = feature gate **and** namespace **and** doc marker (belt +
suspenders).** Rust stable has no `#[unstable]` attribute. Mark experimental three ways so it is
impossible to depend on the experimental tier by accident (AC3):
  1. **Cargo feature `experimental`** (off by default) gates the whole online-runtime surface. A
     default-features consumer cannot even *name* an experimental type. This is the load-bearing
     marker.
  2. **Namespace** `iroh_rooms::experimental::{session, sync, store, blob, pipe_runtime}` — the path
     itself reads as a warning at every call site.
  3. **Doc marker**: each experimental item's doc starts with `**Experimental (unstable API).**` and
     the module carries `#![doc(cfg(feature = "experimental"))]` (via `doc_cfg` on docs.rs) so the
     rendered docs badge it.

**D5 — Re-export, do not re-wrap (initially).** The façade modules are thin `pub use` re-export
files, not new newtypes/wrappers. Rationale: (a) zero behavior/round-trip risk; (b) a consumer's
`iroh_rooms::events::WireEvent` is *the same type* as `iroh_rooms_core::event::WireEvent`, so mixing
the façade with a direct `core` dep never produces "two different `WireEvent`s"; (c) the CLI can
migrate incrementally. Ergonomic wrappers (e.g. a `Room` handle bundling engine+store, an `Identity`
that owns key persistence) are a **deliberate follow-up** once the raw surface is proven (§13 R4,
§15 OQ5) — this issue defines the *boundary*, not a new ergonomic layer.

**D6 — A `prelude` for the stable surface only.** `iroh_rooms::prelude` glob-re-exports the most-used
stable types (`RoomId`, `EventId`, `WireEvent`, `SigningKey`, `IdentityKey`, `Content`, `EventType`,
`RejectReason`, `Role`, `Status`, `RoomInviteTicket`, the `build_*` fns). The prelude never includes
experimental items — so `use iroh_rooms::prelude::*` can never pull in an unstable type.

**D7 — `#![deny(missing_docs)]` on the façade.** Every re-export module and (re-exported) item is
reachable-documented at the façade level via module docs; the compiler enforces that the SDK entry
points are documented (AC1). `core`/`net` keep their existing doc posture (they already document
heavily); the façade's own module docs are the SDK's front door.

**D8 — Version + stability policy in crate docs.** The façade starts at `0.1.0`. Document a plain
semver-for-0.x policy: within `0.x`, the **stable** tier changes only on a minor bump and with a
CHANGELOG entry + deprecation window where feasible; the **experimental** tier may change on any
release. This is the "stable-ish" the issue asks for, honestly scoped for a pre-1.0 preview.

**D9 — Examples are cargo `examples/`, gated by tier.** Offline examples (identity, authoring,
validate, ticket, fold) need only default features and always compile in CI. Online examples (send
over the network, fetch a file, expose a pipe) require `--features experimental` and are compiled
(not executed) in CI via `cargo build --examples --features experimental` (they need two live peers
to *run*, mirroring the `#[ignore]`-gated online test tier). Each example is a self-contained
`main()` that mirrors one getting-started step.

---

## 5. API model (the heart of this spec)

### 5.1 Crate + feature layout

```
crates/iroh-rooms/                     # package "iroh-rooms", lib crate, publish = false (initially)
  Cargo.toml
  src/lib.rs                           # crate docs, stability policy, #![deny(missing_docs)], prelude, module decls
  src/identity.rs                      # stable
  src/room.rs                          # stable
  src/events.rs                        # stable
  src/files.rs                         # stable
  src/pipes.rs                         # stable  (event-authoring side of pipes)
  src/experimental/mod.rs              # #[cfg(feature = "experimental")]
  src/experimental/session.rs          #   Node + transport + admission + conn state
  src/experimental/sync.rs             #   SyncEngine + config + messages + transport trait
  src/experimental/store.rs            #   EventStore + rows
  src/experimental/blob.rs             #   BlobStore + serve/fetch runtime
  src/experimental/pipe_runtime.rs     #   PipeForwarder/PipeRegistry + audit
  examples/01_identity.rs              # default features
  examples/02_create_room.rs           # default features
  examples/03_invite_and_join.rs       # --features experimental (online)
  examples/04_send_message.rs          # --features experimental (online)
  examples/05_share_and_fetch_file.rs  # --features experimental (online)
  examples/06_live_pipe.rs             # --features experimental (online)
  examples/07_agent_status.rs          # --features experimental (online) — the "example agent" seed
  examples/offline_author_and_validate.rs  # default features — pure protocol, no network
```

`Cargo.toml` (shape):

```toml
[package]
name = "iroh-rooms"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true
publish = false            # developer preview; crates.io release is a follow-up (OQ3)
# Deliberately does NOT inherit workspace rust-version: the experimental tier pulls in
# iroh-rooms-net → iroh 1.0 (rustc >= 1.85), mirroring iroh-rooms-net/Cargo.toml.

[features]
default = []                                   # stable surface only
experimental = ["dep:iroh-rooms-net", "iroh-rooms-core/sync"]
full = ["experimental"]

[dependencies]
iroh-rooms-core = { path = "../iroh-rooms-core" }             # stable tier: no store/sync feature
iroh-rooms-net  = { path = "../iroh-rooms-net", optional = true }  # experimental tier only

[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }  # for online examples' main()
```

> Note: the stable tier depends on `iroh-rooms-core` with **no** features (pure protocol). The
> experimental tier turns on `iroh-rooms-core/sync` (→ `store`) and adds `iroh-rooms-net`.

### 5.2 Stable surface (default features)

Each module is a re-export file with a `//!` doc. The complete stable re-export set:

**`iroh_rooms::identity`** — participant + device keys and the device binding.
```rust
pub use iroh_rooms_core::event::keys::{SigningKey, IdentityKey, DeviceKey, Signature};
pub use iroh_rooms_core::event::binding::DeviceBinding;
```
> Doc note: MVP is **one device per identity**. The surface intentionally exposes no device-*set* or
> device-management API (AC4 — multi-device is post-MVP, PRD §13.4/§13.5).

**`iroh_rooms::events`** — the signed-event trust boundary + authoring + validation.
```rust
pub use iroh_rooms_core::event::ids::{EventId, RoomId, HashRef};
pub use iroh_rooms_core::event::wire::WireEvent;
pub use iroh_rooms_core::event::signed::SignedEvent;
pub use iroh_rooms_core::event::content::{Content, EventType, capability_hash};
pub use iroh_rooms_core::event::validate::{
    validate_wire_bytes, validate_with_membership, ValidatedEvent, ValidationContext,
};
pub use iroh_rooms_core::event::reject::{RejectReason, Flag, MembershipOracle};
pub use iroh_rooms_core::event::{build_message_text, build_agent_status};
pub use iroh_rooms_core::PROTOCOL_VERSION;
pub mod constants { pub use iroh_rooms_core::event::constants::*; }  // the MAX_* trust-boundary bounds
```
> `event::cbor` (the raw codec) is **not** re-exported — it is an implementation detail; consumers
> operate on `WireEvent`/`validate_wire_bytes`, never raw CBOR (§13 R1).

**`iroh_rooms::room`** — room lifecycle, membership fold, capabilities.
```rust
pub use iroh_rooms_core::event::ids::RoomId;   // also surfaced here for discoverability
pub use iroh_rooms_core::event::{
    build_room_created, build_member_invited, build_member_joined,
    build_member_left, build_member_removed,
};
pub use iroh_rooms_core::membership::{
    RoomMembership, MembershipSnapshot, Member, Role, Status, Ingest, AncestorView,
    blob_serve_allowed, pipe_connect_allowed, BlobDecision, PipeDecision, DenyReason,
};
pub use iroh_rooms_core::ticket::{RoomInviteTicket, TicketError};
```

**`iroh_rooms::files`** — the blob-reference event side (authoring/validation of `file.shared`).
```rust
pub use iroh_rooms_core::event::build_file_shared;
pub use iroh_rooms_core::event::content::FileShared;
pub use iroh_rooms_core::event::ids::HashRef;
```
> The *runtime* half (import/serve/fetch) is experimental (`experimental::blob`). Stable `files`
> covers authoring a reference and validating one received.

**`iroh_rooms::pipes`** — the pipe-event side (authoring/validation of `pipe.opened`/`pipe.closed`).
```rust
pub use iroh_rooms_core::event::{build_pipe_opened, build_pipe_closed};
pub use iroh_rooms_core::event::content::{PipeOpened, PipeClosed};  // both structs are pub in core::event::content
```
> The *forwarding* runtime (`PipeForwarder`/`PipeRegistry`/splice) is experimental
> (`experimental::pipe_runtime`).

**`iroh_rooms::prelude`** — glob of the most-used stable items (D6).

### 5.3 Experimental surface (`--features experimental`)

`iroh_rooms::experimental` (module carries the doc-cfg badge and an `//! **Experimental.**` banner):

**`experimental::session`** — the online runtime.
```rust
pub use iroh_rooms_net::{
    Node, BlobServeConfig, DEFAULT_TICK,
    NetTransport, NetConfig, NetMode, Inbound, Shared,
    Admission, AdmissionDecision, AdmissionView, AllowlistAdmission,
    JoinBootstrapAdmission, SnapshotAdmission, RejectCause,
    PeerManager, PeerConnState, ConnEvent, OfflineReason, PeerEntry, PeerTable,
    AuditSink, BlobDenyCause, TracingAudit, EventProtocolHandler, EVENT_ALPN,
};
```

**`experimental::sync`** — the sans-IO engine + transport trait.
```rust
pub use iroh_rooms_core::sync::{
    SyncEngine, SyncConfig, SyncMessage, SyncTransport, Completeness, Severity,
    SyncCounters, SyncDigest, SyncError, TrustDecision, Outgoing, PeerId, Window, WireBytes, MessageError,
};
// sync::sim (the test harness) is intentionally NOT re-exported.
```

**`experimental::store`** — local persistence.
```rust
pub use iroh_rooms_core::store::{
    EventStore, StoredEvent, StoreError, InsertOutcome, InsertStats,
    ParkedRow, SyncStateRow, TrustRow,
};
```

**`experimental::blob`** — blob import / serve / verified fetch.
```rust
pub use iroh_rooms_net::{BlobStore, BlobAclView, BlobError, BlobImport, FetchOutcome};
```

**`experimental::pipe_runtime`** — live-pipe forwarding.
```rust
pub use iroh_rooms_net::{
    PipeForwarder, PipeRegistry, PipeOutcome, PipeError, PipeDenyCause,
    PipeAuditSink, TracingPipeAudit, new_pipe_id, PIPE_ALPN,
};
```

### 5.4 CLI-uses-the-same-APIs (AC2) — audit + conformance

Two-part evidence, graduated so risk stays bounded:

1. **Coverage audit (required).** Produce a table (in the crate docs or a `docs/sdk-coverage.md`)
   mapping every `use iroh_rooms_core::` / `use iroh_rooms_net::` symbol in `crates/iroh-rooms-cli/src`
   to its façade path (stable or `experimental::*`), or to an explicit **"CLI-internal, not in the
   SDK"** list with a one-line reason. The known CLI-internal residue to expect on that list:
   - `iroh_rooms_net::pipe::is_loopback_target` — a loopback-guard helper; promote to
     `experimental::pipe_runtime` **or** keep CLI-internal (recommend promote; it is a natural SDK
     helper). §15 OQ6.
   - any use of `event::cbor` (none expected — confirm) and `sync::sim` (test-only) stay out.
   The audit *is* the AC2 artifact: it proves the façade is a superset of the CLI's real needs
   ("where practical").
2. **Conformance migration (required, bounded).** Migrate the CLI's **offline authoring path**
   (`identity.rs`, `room.rs` create/members, `invite.rs`, and the `build_*` call sites in
   `message.rs`/`file.rs`) to import stable items from `iroh-rooms` instead of `iroh-rooms-core`
   directly, and add `iroh-rooms` (with `experimental`) to the CLI's `Cargo.toml`. Because re-exports
   are the same types (D5), this is a mechanical import swap with **no behavior change** — every
   existing CLI test (`room_cli.rs`, `tail_cli.rs`, `file_cli.rs`, `two_peer_e2e.rs`,
   `full_demo_e2e.rs`, …) must stay green unchanged. Migrating the online path (`Node`, engine) to
   `experimental::*` is **recommended but optional** this issue; whatever is not migrated stays a
   direct `core`/`net` dep and is listed in the audit. This satisfies the Test Plan's "verify CLI
   continues using core APIs" while proving the surface is real.

> Rationale for not force-migrating 100% of the CLI: the CLI is the reference consumer, but a
> big-bang rewrite is churn with no functional payoff and raises risk. The audit proves *coverage*;
> the partial migration proves *consumability*. Full migration can follow once the surface settles.

---

## 6. Implementation steps

1. **Scaffold the crate.** Create `crates/iroh-rooms/` with `Cargo.toml` (§5.1) and add it to the
   root `Cargo.toml` `[workspace] members`. `src/lib.rs`: crate-level `//!` docs (what the SDK is,
   the stable/experimental tiers, the D8 stability policy, a "start here" pointer to the demo
   examples), `#![deny(missing_docs)]`, `#![cfg_attr(docsrs, feature(doc_cfg))]`, module declarations,
   and `pub mod prelude`.
2. **Stable modules.** Add `identity.rs`, `events.rs`, `room.rs`, `files.rs`, `pipes.rs` as the
   re-export files in §5.2, each with a `//!` doc paragraph and per-item doc lines where the
   re-export name benefits from SDK-level framing (e.g. "MVP is single-device"). Verify each
   re-exported path exists against the current `core` surface (§2.1) before writing it.
3. **Experimental modules.** Add `experimental/mod.rs` (`#[cfg(feature = "experimental")]`,
   `//! **Experimental (unstable API).**`, `#![doc(cfg(feature = "experimental"))]`) and the five
   submodules in §5.3. Each experimental item's doc opens with the `**Experimental.**` marker (D4).
4. **Prelude.** Implement `prelude` (D6) — stable items only; add a doctest that `use
   iroh_rooms::prelude::*;` compiles.
5. **Doctests.** Add a compilable `` ```rust `` example to each stable module's `//!` doc (offline,
   no network) — e.g. `events` doc authors + validates a `message.text`; `room` doc folds a
   two-event log; `files` doc builds + validates a `file.shared`. These run under `cargo test --doc`
   (default features).
6. **Examples.** Add `examples/*.rs` (§5.1). Offline examples (`01`, `02`, `offline_author_and_validate`)
   use default features. Online examples (`03`–`07`) begin with
   `//! Requires: --features experimental` and a `#[cfg(feature = "experimental")]` gate on `main`
   (with a `#[cfg(not(...))] fn main() { eprintln!("build with --features experimental") }` stub so
   `cargo build --examples` on default features still succeeds). Mirror the corresponding
   `docs/getting-started.md` step; keep them copy-pasteable and comment the placeholders.
7. **`net` publishability decision (OQ3).** Keep `iroh-rooms` `publish = false` for the preview (no
   `net` change needed). If the maintainer wants crates.io, that is a separate follow-up that must
   also flip `iroh-rooms-net`'s `publish` and reconcile the MSRV note — out of scope here.
8. **Doc the implementation crates.** Add a short `//!` note to `iroh-rooms-core/src/lib.rs` and
   `iroh-rooms-net/src/lib.rs`: "This is an implementation crate. The supported SDK entry point is the
   `iroh-rooms` façade; items here may change. Depend on this crate directly only for advanced use."
   (Additive doc only — no code change.)
9. **CLI coverage audit + partial migration** (§5.4): write the coverage table, add the `iroh-rooms`
   dependency to `crates/iroh-rooms-cli/Cargo.toml`, and swap the offline-authoring import sites.
   Run the full CLI test suite unchanged.
10. **`verify.sh` / CI.** Ensure `scripts/verify.sh` builds the new crate under `--all-features`
    (fmt `--check`, clippy `-D warnings` pedantic, tests) and compiles examples in **both** feature
    configurations: `cargo build -p iroh-rooms --examples` (default) and
    `cargo build -p iroh-rooms --examples --features experimental`. Add a line to `docs/` or the
    README "Repository Layout" noting the new crate.
11. **CHANGELOG / docs.** Seed a `crates/iroh-rooms/CHANGELOG.md` (0.1.0 — initial surface) and, if a
    docs index exists, link the new library docs from `docs/getting-started.md` ("Using it as a
    library").

---

## 7. Validation & authorization rules

**None new.** The SDK re-exports the *same* validation (`validate_wire_bytes` /
`validate_with_membership`) and authorization (the membership fold gates + access predicates) the CLI
and transport already enforce. The façade adds no path that bypasses them; because it re-exports
concrete types (D5), a `WireEvent` produced via the façade is validated by the identical pipeline.
The only "rule" this issue introduces is the **stability contract** (D8) — a documentation-level
promise, compiler-enforced only insofar as `#![deny(missing_docs)]` and the `experimental` feature
gate keep the tiers honest.

---

## 8. Error model & observability

- **Errors:** the SDK surfaces the existing typed error enums as-is — stable `RejectReason` /
  `TicketError`; experimental `StoreError` / `SyncError` / `BlobError` / `PipeError` /
  `FrameError` / `MessageError`. The façade does **not** introduce a unifying `enum SdkError`
  (that would hide the taxonomy the CLI error mapping — IR-0110 — deliberately branches on). Document
  in the crate docs how these map to the CLI's exit-code taxonomy so a library consumer can build the
  same UX.
- **Observability:** unchanged. The experimental runtime keeps its `AuditSink` /
  `TracingAudit` / `PipeAuditSink` hooks (re-exported in `experimental::session` /
  `experimental::pipe_runtime`). Reminder for example authors: the library emits `tracing` events but
  installs **no** subscriber (per the "CLI has no tracing subscriber" project note); online examples
  that want to *see* audit output must install a subscriber or an explicit sink themselves — document
  this in the online examples' headers.

---

## 9. Security / privacy / reliability

- **No new trust surface.** Re-exports only; every crypto/authorization guarantee is inherited. A
  library consumer cannot reach an event into a store or onto the wire without going through the same
  `validate_*` + fold gates.
- **Secret hygiene.** `SigningKey` already zeroizes on drop and has a redacted `Debug`; the ticket
  type redacts its secret. **Examples and doctests must never print secret material** (no `{:?}` on a
  `SigningKey`'s secret, no printing a raw ticket secret) — call this out in the example-authoring
  checklist and assert it in review. The stable surface exposes no accessor that returns raw secret
  bytes beyond what `core` already offers (`to_seed`, itself zeroizing).
- **Availability honesty (AC4-adjacent).** The experimental `session`/`sync` docs must state the PRD
  §14 model verbatim (best-effort, no central inbox, no guaranteed offline delivery) so the API shape
  does not *imply* a queue/guaranteed-delivery capability that does not exist.
- **Reliability.** Purely additive crate; no change to any persisted schema or wire format, so no new
  corruption/interop surface. The Gate-A real-NAT residual is unchanged and remains a property of the
  (experimental) transport, correctly reflected by its tier.

---

## 10. Migration / rollout / rollback

- **Rollout:** additive. New crate + additive doc lines + optional CLI import swap. Nothing existing
  changes behavior. Ship behind the developer-preview framing (Phase 2).
- **Backwards compatibility:** the CLI import migration is type-identical (D5); all existing tests
  must pass unchanged — that is the regression gate.
- **Rollback:** delete `crates/iroh-rooms/`, remove it from the workspace members, revert the CLI
  import swap and the additive doc lines. No migration, no persisted-state change, no wire change.
- **Forward path (out of scope, documented):** (i) ergonomic wrappers (`Room`/`Identity` handles);
  (ii) narrowing `core`/`net` visibility once the façade is proven; (iii) crates.io publication +
  MSRV reconciliation; (iv) graduating specific experimental items to stable as they settle.

---

## 11. Test plan

The Test Plan in the issue is "compile examples and verify CLI continues using core APIs." Concretely:

- **L1 — Crate builds, both tiers.** CI: `cargo build -p iroh-rooms` (default) and
  `cargo build -p iroh-rooms --features experimental`; `scripts/verify.sh` (`--all-features`) covers
  fmt/clippy/test. A `lib.rs` `#[test]` asserts `PROTOCOL_VERSION` is re-exported and equals `1`
  (a trivial tripwire that the stable re-export path resolves).
- **L2 — Doctests.** `cargo test -p iroh-rooms --doc` (default features) compiles+runs every stable
  module's `//!` example (offline author + validate, fold, ticket round-trip). `--features
  experimental` doctests compile the experimental module intro (no network in a doctest — keep
  experimental doctests `no_run` where they would dial).
- **L3 — Examples compile (AC1 "documented with examples").** CI:
  `cargo build -p iroh-rooms --examples` (default — offline examples real, online examples hit the
  `cfg(not(experimental))` stub) **and** `cargo build -p iroh-rooms --examples --features
  experimental` (all examples compiled). Optionally run the offline examples as `#[test]`-wrapped
  smoke checks; online examples are compile-only in CI (they need two live peers, like the existing
  `#[ignore]` online tier) — **log this coverage boundary** so "examples compile" is not mistaken for
  "online examples run in CI".
- **L4 — CLI conformance (AC2).** After the coverage audit + partial migration (§5.4):
  - the coverage table is checked in and every CLI `core`/`net` import is either mapped to a façade
    path or on the explicit CLI-internal list (a simple `grep`-based test or a review checklist);
  - `cargo test -p iroh-rooms-cli` passes **unchanged** (`room_cli.rs`, `tail_cli.rs`, `file_cli.rs`,
    `agent_cli.rs`, `two_peer_e2e.rs`, `full_demo_e2e.rs`, …) — proving the swap is behavior-neutral;
  - a new `crates/iroh-rooms/tests/cli_surface.rs` (or a doc-example) reconstructs the CLI's core
    offline flow (create room → invite → build+validate a `message.text` → fold membership) using
    **only** `iroh_rooms::{room, events, identity}` stable imports, asserting the façade alone is
    sufficient for the offline authoring path.
- **L5 — Experimental gating (AC3).** A compile-fail check (documented, or a `trybuild` case if the
  team uses it) that `iroh_rooms::experimental::session::Node` is **not** nameable without the
  feature; and that `use iroh_rooms::prelude::*;` never brings an experimental type into scope.
- **L6 — No-post-MVP shape (AC4).** A review checklist (encoded as a doc section) asserting the
  surface exposes no device-set/management API, no call-plane type, no availability-layer type, and
  that experimental `session`/`sync` docs carry the §14 availability disclaimer. (Mechanical: `grep`
  the façade for any `device`s-plural / `call` / `archive` / `pin` / `queue` surface — none should
  exist.)

**Gate:** `scripts/verify.sh` (fmt `--check` + clippy `-D warnings` pedantic + full test) is the real
CI gate; `cargo test` alone is not green (project "verify.sh is the real CI gate" note).

---

## 12. Acceptance criteria → evidence

| Issue AC | Satisfied by | Test |
|---|---|---|
| SDK surface is documented with examples | Façade crate docs + `#![deny(missing_docs)]` (D7) + per-module `//!` + `examples/*.rs` mirroring the demo (D9, §5.1, §6) | L1, L2, L3 |
| CLI uses the same public core APIs where practical | Coverage audit table + partial CLI migration to `iroh-rooms` (§5.4); every CLI test green unchanged | L4 |
| Experimental APIs are clearly marked | `experimental` cargo feature + `experimental::*` namespace + `**Experimental.**` doc marker + `doc(cfg)` badge (D4) | L5 |
| No post-MVP capabilities implied by the API shape | Stable = offline protocol only; online runtime is experimental & narrowly shaped; no device-set/call/availability surface (D3, §3, §5) | L6 |

---

## 13. Risks

- **R1 — Committing a churny type as stable.** If a "stable" re-export turns out to need breaking
  changes, the promise is broken. *Mitigation:* stable ⊆ the conformance-tested, byte-stable protocol
  layer (D3); the raw codec (`event::cbor`) and all IO/schema types are excluded; 0.x policy allows a
  minor-bump break with a deprecation window (D8).
- **R2 — The boundary is "illusory" because `core`/`net` stay fully `pub`.** A determined consumer
  can still `iroh_rooms_core::…` around the façade. *Mitigation:* accepted for the initial surface
  (§3 non-goal); the boundary is established by the advertised entry point + the experimental feature
  gate + implementation-crate disclaimers (step 8). Visibility narrowing is a scoped follow-up (OQ4)
  once the façade proves sufficient — doing it now would be a large, risky change mid-preview.
- **R3 — Re-export path drift.** A `pub use` naming a path that later moves silently breaks the SDK.
  *Mitigation:* every path in §5.2/§5.3 is verified against the current surface (§2.1) at
  implementation time; the L1 tripwire test + doctests + examples fail to compile if a path is wrong,
  so drift is caught by CI, not consumers.
- **R4 — Under-serving ergonomics.** Thin re-exports (D5) may feel low-level (a consumer still wires
  engine+store+transport by hand, as the CLI does). *Mitigation:* explicitly a follow-up
  (`Room`/`Identity` wrappers, OQ5); this issue's job is the *boundary*, and the examples show the
  end-to-end wiring so it is at least discoverable.
- **R5 — MSRV / publish coupling.** The experimental tier pulls in `iroh-rooms-net` (rustc ≥ 1.85,
  `publish = false`), so the façade cannot be crates.io-published while re-exporting it. *Mitigation:*
  `publish = false` initially (OQ3); the stable-only tier could be published independently later if
  desired (it depends only on the publishable `iroh-rooms-core`).
- **R6 — CLI migration regressions.** Swapping imports could subtly change resolution (e.g. a name
  that exists in two modules). *Mitigation:* type-identical re-exports (D5); migrate only the offline
  authoring path; require the entire existing CLI test suite green unchanged (L4).

## 14. Assumptions

- The `core`/`net` public surfaces enumerated in §2.1 are exactly as shipped on this branch
  (verified against `lib.rs`/`mod.rs` at spec time); the compiled crates remain the source of truth
  and any path is re-verified at implementation.
- "Stable-ish" (issue Scope) means a documented 0.x semver policy with a favored-stable tier, not a
  1.0 guarantee (D8).
- A new workspace crate is acceptable (D1); if not, fall back to annotating the existing surface in
  place (OQ1).
- Phase 2 delivers via path/git dependency, not crates.io (§3, OQ3).
- No GitHub/network/git actions are performed by this planning phase (the ADW orchestrator owns
  git/gh).

## 15. Open questions

- **OQ1 — Façade crate vs annotate-in-place.** Recommend the façade (D1). If a maintainer vetoes a
  new crate, fall back to: add the `experimental` feature + doc markers directly to `core`/`net` and
  publish a `docs/sdk.md` that names the supported subset. (Weaker on AC4 — cannot shape prominence.)
- **OQ2 — Crate name.** `iroh-rooms` (recommended, D2) vs `iroh-rooms-sdk`. `iroh-rooms` is the
  cleaner "one dependency" name and does not collide with the `iroh-rooms-cli` package.
- **OQ3 — Publishability.** Keep `publish = false` for the preview (recommended), or invest now in
  making the chain crates.io-publishable (flip `iroh-rooms-net` publish + reconcile the workspace
  MSRV note). Recommend defer.
- **OQ4 — Narrow `core`/`net` visibility later?** Whether/when to demote implementation internals to
  `pub(crate)` or a sealed surface. Recommend a separate follow-up after the façade is proven, not
  this issue (R2).
- **OQ5 — Ergonomic wrappers.** Should the *initial* surface include a `Room`/`Identity` convenience
  layer, or ship thin re-exports first? Recommend thin re-exports now (D5), wrappers as a fast
  follow-up informed by the example-writing experience.
- **OQ6 — Promote `is_loopback_target` (and any other CLI-internal helper the audit surfaces) into
  `experimental::pipe_runtime`?** Recommend promote helpers that are genuinely useful to a library
  consumer; keep test-only/loopback-hack helpers CLI-internal and list them in the coverage audit.
- **OQ7 — Which examples run vs compile-only in CI.** Offline examples run; online examples
  compile-only (need two live peers). Confirm that boundary is acceptable and logged (L3), matching
  the existing `#[ignore]` online-test convention.
