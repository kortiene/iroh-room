# Changelog

All notable changes to the `iroh-rooms` SDK façade are documented here. See
`src/lib.rs` for the versioning policy: within `0.x`, the **stable** tier
changes only on a minor bump (with an entry here and a deprecation window
where feasible); the **experimental** tier may change on any release.

## Unreleased

- Removed the membership-sync room-size ceiling (issue #113, `iroh-rooms-core`
  / `iroh-rooms-net`): the `WantMembership` requester claimed **every held
  event id** in `have` (required by #111's progress invariant), so at ~30k held
  events the request exceeded the 1 MiB wire frame cap, was dropped at the net
  writer, and membership anti-entropy to that peer silently stalled. The `have`
  entries are now bounded **ancestry claims** — the requester samples its held
  set (placed DAG heads, a recent-lamport slab, and a per-tick rotating window
  over older history; ≤ `membership_have_max_ids`, default 512, ~17 KiB), and
  the responder subtracts each claimed id *plus every stored ancestor of it*.
  An old-style exhaustive claim over an intact store is causally closed and
  expands to exactly itself, so rc.2 requesters are served identically (see the
  upgrade note for the store-hole exception). Claims never include
  causally-unplaced (`NULL`-lamport) rows, so a local store hole keeps being
  re-served until it heals; the rotating window guarantees a claim cannot stay
  pinned in peer-unknown territory (an offline suffix deeper than the whole
  budget anchors within at most `placed-events` ticks). `Events` responses are
  now **byte-budgeted**: a serve larger than one wire frame is split into
  consecutive under-cap messages instead of being dropped whole and re-served
  forever (previously reachable at any room size via ~64 near-16-KiB message
  bodies in the membership ancestry). `SyncEngine::publish` now refuses a
  locally-authored frame too large to ever deliver
  (`SyncError::OversizedFrame`), and the Gate-D `SimNet` enforces the frame cap
  at delivery so this failure class stays visible to the deterministic tests.
  **Upgrade note: a v0.1.0-rc.2 responder subtracts the new bounded claim as an
  exact id set, so a fresh bootstrap against an old responder hard-stalls once
  the joiner holds more than `membership_have_max_ids` + `response_max_frames`
  (~1k) events — every room member, especially the admin, must run a build with
  this fix for rooms past that size. Two rc.2 residuals this fix cannot reach:
  an rc.2 requester whose store has a hole (a swallowed insert error) claims
  the unplaced rows above it, so an upgraded responder covers — and never
  re-serves — the missing ancestor that rc.2-to-rc.2 exact-set subtraction
  would have healed; and an oversized event that entered an rc.2 log before
  the publish guard existed still re-serves-and-drops on every pull to that
  peer (now logged at the responder).**

## 0.1.0-rc.2 - 2026-07-15

- Fixed the join-after-conversation deadlock (PR #111, `iroh-rooms-core` /
  `iroh-rooms-net` / `iroh-rooms-cli`): once any non-admin chat existed in a
  room, no new participant could ever complete `room join` — the invite cites
  the current DAG heads (chat events after a conversation), the membership fold
  requires every `prev_events` parent before classifying, `WantMembership`
  served only the bare authorization class, and the admin drops `WantEvents`
  backfill from provisional peers, a circular deadlock ending in a 10s timeout.
  `WantMembership` now serves the **causal closure** of the authorization class
  (memoized, room-scoped), and the requester's `have` claims every held event
  id, giving guaranteed `ceil(closure/cap)`-round bootstrap progress under the
  512-frame response cap. The net writer now drops a locally-queued oversized
  frame instead of killing the peer stream, and `room join` distinguishes a new
  `membership_incomplete` error (admin responded, ancestry never completed —
  counted per-attempt) from `no_admin_reachable`. Known residuals tracked in
  issues #112 (provisional closure read without capability proof), #113
  (have-list frame ceiling ~30k events), #114 (offline-member deep-chat-gap
  wedge). **Upgrade note: a v0.1.0-rc.1 admin still serves the bare class, so
  joins minted after a conversation keep failing in mixed-version rooms — every
  room member, especially the admin, must run rc.2.**
- Hardened cross-room isolation in the sync engine (PR #106,
  `iroh-rooms-core`): every event-id lookup against the shared event store is
  now room-scoped. Because the store holds every room in one database and
  `event_id` is a globally-unique content hash, unscoped lookups let a row from
  another room be served to a peer via `WantEvents` (cross-room byte
  disclosure), satisfy a local causal dependency, or clear the fail-closed
  admin-tip suspect state. New room-scoped store methods (`contains_in_room` /
  `get_in_room` / `missing_parents_in_room`) close all three. Since `event_id`
  is a unique primary key the scoping is a pure narrowing — legitimate same-room
  sync is unchanged and the reads stay PK point lookups (perf-neutral). No
  façade API change; a behavioral security fix that flows through to any
  online-tier consumer. Regression-tested at both the store and sync-engine
  layers.
- Added a compile-time `relay-only-test` cargo feature (PR #107,
  `iroh-rooms-net` with a façade pass-through) and re-exported the
  `RELAY_ONLY_TEST_BUILD` build-flavor constant through `experimental::session`.
  With the feature on, a `RealNetwork` endpoint suppresses direct UDP transports
  (`clear_ip_transports()`) so all room, blob, and pipe traffic traverses the
  configured relay — a controlled seam for Gate-A relay-throughput
  verification. Off by default and compile-time only, so ordinary binaries
  cannot switch transport policy at runtime and default behavior is unchanged.
  Note: the feature is deliberately non-additive and is enabled by
  `--all-features`; it is dormant under `cargo test` today (no non-ignored
  `RealNetwork` test), but a future such test must gate the seam behind a
  runtime switch to avoid forcing relay-only in CI.

## 0.1.0-rc.1 - 2026-07-07

- Re-exported the online tier's `iroh` transport identities — `EndpointAddr`,
  `EndpointId`, `SecretKey`, `Endpoint` — from `experimental::session`
  (`EndpointId` also from `experimental::blob` and `experimental::pipe_runtime`,
  issue #87): closes the last gap in "a consumer imports only through
  `iroh_rooms::*`". Driving `Node::spawn`/`connect_to`/admission wiring
  previously required a consumer's own direct `iroh` dependency pinned
  byte-identical to `iroh-rooms-net`'s `=1.0.1` — a version-skew trap where
  two resolved `iroh` crates produce incompatible `EndpointAddr` types. `iroh`
  becomes a direct, `experimental`-gated optional dependency of the façade
  (pinned `=1.0.1` to match `-net` exactly, so Cargo unifies to one crate
  instance); a default-features build still cannot name any of these types.
  The reference CLI proves the claim: its direct `iroh` dependency is deleted
  entirely, with every `iroh::` path routed through the façade instead. Purely
  additive — a re-export + import-routing change, no new runtime behavior.
- Added `Node::live_pipe_sessions_for(pipe_id) -> usize` and
  `Node::pipe_session_info() -> Vec<PipeSessionInfo>` (issue #86 / IR-0309,
  `experimental::session` + `experimental::pipe_runtime`): per-pipe
  live-session observability on the owner side, so an owner exposing more
  than one pipe can tell which pipe carries a live forwarding session
  instead of only a node-wide total (`Node::live_pipe_sessions()`). Both are
  pure `&self` reads over the existing session table — no new tracking, no
  engine/pump involvement — and are decrement-correct on every teardown path
  with no separate counter to desync. `live_pipe_sessions()` is unchanged;
  purely additive.
- Added `Node::blob_import(&Path)` / `Node::blob_import_bytes(Bytes)` (issue #84 /
  IR-0308, `experimental::session` + `experimental::blob`): import a file, or
  re-provide in-memory bytes, into the live session's already-open blob store —
  no second `FsStore` open (so no `BlobError::Locked`), no session cycle, zero
  `ConnEvent` disconnects. Pair with `build_file_shared` + `Node::publish` to
  announce the reference. A node spawned without a `BlobServeConfig` returns
  the new `BlobError::NotServing`. Purely additive; existing `Node` methods and
  the exclusive-lock model are unchanged.
- Added `Node::room_events() -> broadcast::Receiver<StoredEvent>` (issue #83 /
  IR-0307, `experimental::session`): a live push stream of every event accepted
  into the room's store — own publish, peer sync, and delayed park-promotion
  all emit here exactly once, so a long-running consumer (e.g. a resident
  daemon driving a UI) no longer has to poll `room_tail`. Lossy on lag like
  `conn_events` (`RecvError::Lagged`, resync via `room_tail` + a seen-set —
  see the method's doc comment for the recipe). Purely additive; existing
  `Node` methods are unchanged.
- Added `examples/example_agent/` (issue #39 / IR-0304): a minimal, runnable
  example agent driven by real command-line arguments — the adapt-me-as-a-
  template evolution of `07_agent_status.rs` — plus a co-located `README.md`
  and a gated integration test. Docs-and-examples only; no SDK surface change.
- Added `JoinBootstrapAdmission::new_dynamic` (issue #88, `experimental::session`):
  the join-bootstrap window (`accept_joins`) can now be read from a shared
  `Arc<AtomicBool>` on every `authorize()` call instead of being fixed at
  construction, so a long-running host (e.g. a resident daemon) can gate
  provisional admission on pending invites without respawning its `Node`.
  Purely additive — `new` and its fixed-`bool` semantics are unchanged, and
  `new_dynamic` is observationally identical to `new` for any fixed flag
  value.

## 0.1.0 — initial surface (IR-0301)

Initial developer-preview release. Defines the SDK boundary:

- Five stable domain modules — `identity`, `room`, `events`, `files`, `pipes`
  — re-exporting the deterministic, conformance-tested protocol layer from
  `iroh-rooms-core` (event authoring/validation, the membership fold, the
  invite ticket codec).
- An `experimental` cargo feature gating the online runtime — `session`
  (transport/admission/connection state), `sync` (the sans-IO engine), `store`
  (the local event store), `blob` (import/serve/fetch), `pipe_runtime`
  (live-pipe forwarding) — re-exported from `iroh-rooms-net` /
  `iroh-rooms-core`.
- A `prelude` module glob-re-exporting the most-used stable types.
- `examples/` mirroring the `docs/getting-started.md` demo, plus doctests on
  every stable module.
- The CLI (`iroh-rooms-cli`) migrated its offline authoring path
  (`identity`, `room` create/members, `invite`, and the `build_*` call sites
  in `message`/`file`) to import through this façade — see
  `docs/sdk-coverage.md` for the full coverage audit.

No crates.io publication yet (`publish = false`); no stability guarantee on
the `experimental` tier.
