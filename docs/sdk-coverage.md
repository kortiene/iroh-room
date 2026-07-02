# SDK coverage audit (IR-0301)

This is the AC2 evidence artifact for issue #36 / IR-0301: every symbol
`crates/iroh-rooms-cli/src` imports from `iroh-rooms-core` / `iroh-rooms-net`
(in production code — not `#[cfg(test)] mod tests` blocks, which exercise the
same symbols already listed below), mapped to its `iroh-rooms` façade path.
The façade is a superset of what the CLI needs: every production import maps
to a façade path — there is no CLI-internal residue.

Regenerate this table by re-running the `grep` in the "Method" section below
whenever the CLI's imports change; a path listed here that no longer resolves
is a drift signal (spec R3).

## Stable tier (`iroh_rooms::{identity, room, events, files, pipes}`)

| `core` symbol | Façade path | Used by |
|---|---|---|
| `event::keys::IdentityKey` | `identity::IdentityKey` | audit.rs, display.rs, invite.rs, join.rs, pipe.rs, room.rs |
| `event::keys::DeviceKey` | `identity::DeviceKey` | file.rs |
| `event::keys::SigningKey` | `identity::SigningKey` | identity.rs |
| `event::keys::Signature` | `identity::Signature` | (test-only) |
| `event::binding::DeviceBinding` | `identity::DeviceBinding` | join.rs |
| `event::ids::RoomId` | `room::RoomId` (also `events::RoomId`) | cli.rs, display.rs, file.rs, invite.rs, join.rs, pipe.rs, room.rs |
| `event::ids::EventId` | `events::EventId` | join.rs, message.rs |
| `event::ids::HashRef` | `files::HashRef` (also `events::HashRef`) | file.rs |
| `event::content::{Content, EventType}` | `events::{Content, EventType}` | display.rs, file.rs, join.rs, message.rs, pipe.rs, room.rs |
| `event::content::FileShared` | `files::FileShared` | file.rs |
| `event::signed::SignedEvent` | `events::SignedEvent` | display.rs, file.rs, join.rs, message.rs, pipe.rs, room.rs |
| `event::signed::derive_room_id` | `room::derive_room_id` | room.rs (`create`) |
| `event::validate::{validate_wire_bytes, ValidationContext}` | `events::{validate_wire_bytes, ValidationContext}` | file.rs, join.rs, message.rs, room.rs |
| `event::RejectReason` | `events::RejectReason` | error.rs, join.rs |
| `event::constants::{MAX_MESSAGE_BODY_BYTES, MAX_PREV_EVENTS, MAX_SHARED_FILE_BYTES, MAX_ARTIFACT_REFS, MAX_STATUS_LABEL_BYTES, MAX_STATUS_MESSAGE_BYTES, SHORT_ID_LEN}` | `events::constants::*` | agent.rs, file.rs, invite.rs, join.rs, message.rs |
| `event::build_room_created` | `room::build_room_created` | room.rs |
| `event::build_member_invited` | `room::build_member_invited` | invite.rs |
| `event::build_member_joined` | `room::build_member_joined` | join.rs |
| `event::{build_message_text, build_agent_status}` | `events::{build_message_text, build_agent_status}` | message.rs |
| `event::build_file_shared` | `files::build_file_shared` | file.rs |
| `event::build_pipe_opened` / `build_pipe_closed` | `pipes::build_pipe_opened` / `build_pipe_closed` | (test-only in pipe.rs) |
| `event::capability_hash` | `events::capability_hash` | invite.rs |
| `membership::{Ingest, MembershipSnapshot, Role, RoomMembership, Status}` | `room::{Ingest, MembershipSnapshot, Role, RoomMembership, Status}` | file.rs, invite.rs, join.rs, message.rs, room.rs, display.rs |
| `ticket::{RoomInviteTicket, TicketError}` | `room::{RoomInviteTicket, TicketError}` | error.rs, invite.rs, join.rs |

**Note on `derive_room_id`.** The spec's original §5.2 `room` module listing
did not name this symbol, but the CLI's offline `room create` path computes
`room_id` from `(sender_id, room_nonce, created_at)` *before*
`build_room_created` returns a wire event — this audit surfaced that real
dependency, so `derive_room_id` was added to `iroh_rooms::room` (it is exactly
as pure/deterministic as the already-listed `capability_hash`; see
`crates/iroh-rooms/src/room.rs`).

## Experimental tier (`iroh_rooms::experimental::*`)

| `net`/`core` symbol | Façade path | Used by |
|---|---|---|
| `store::{EventStore, StoredEvent}` | `experimental::store::{EventStore, StoredEvent}` | agent.rs, display.rs, file.rs, invite.rs, join.rs, message.rs, pipe.rs, room.rs |
| `sync::{SyncConfig, SyncEngine}` | `experimental::sync::{SyncConfig, SyncEngine}` | file.rs, join.rs, message.rs, pipe.rs |
| `net::{Admission, AdmissionView, AllowlistAdmission, BlobServeConfig, ConnEvent, JoinBootstrapAdmission, NetConfig, NetMode, Node, PeerConnState, PeerEntry, PeerManager, SnapshotAdmission, DEFAULT_TICK}` | `experimental::session::*` | join.rs, message.rs, pipe.rs |
| `net::{AuditSink, BlobDenyCause, RejectCause}` | `experimental::session::*` | audit.rs |
| `net::OfflineReason` | `experimental::session::OfflineReason` | error.rs |
| `net::{BlobError, BlobStore, FetchOutcome}` | `experimental::blob::*` | file.rs |
| `net::{PipeAuditSink, PipeDenyCause, PipeError, PipeOutcome}` | `experimental::pipe_runtime::*` | pipe.rs |
| `net::pipe::is_loopback_target` | `experimental::pipe_runtime::is_loopback_target` | pipe.rs |

**Resolution of spec OQ6** (promote `is_loopback_target`?): promoted. It is a
small, genuinely reusable loopback-target guard for any consumer building a
pipe connector, not a test-only hack, so it is re-exported from
`experimental::pipe_runtime` rather than left CLI-internal.

## Confirmed absent from the façade (by design)

- `event::cbor` — the raw deterministic-CBOR codec. Confirmed **not** imported
  anywhere in `crates/iroh-rooms-cli/src` (grep returns nothing); consumers
  operate on `WireEvent` / `validate_wire_bytes` only (spec §5.2 note, R1).
- `sync::sim` — the in-memory sync test harness. Confirmed **not** imported by
  the CLI (it is `iroh-rooms-core`'s own deterministic test tool, exercised
  only from `iroh-rooms-core`'s test suite).

## Migration status (AC2 conformance)

Per spec §5.4, only the **offline authoring path** was migrated to import
through the façade (a mechanical, type-identical `pub use` swap — spec D5 —
so every existing CLI test stays green unchanged):

- `identity.rs` — fully migrated (`iroh_rooms::identity::SigningKey`).
- `room.rs` (`create` / `members`) — fully migrated to `iroh_rooms::{events,
  identity, room}`; `store::EventStore` stays a direct `iroh-rooms-core`
  import (the online/persistence tier, optional per §5.4).
- `invite.rs` — fully migrated to `iroh_rooms::{events, identity, room}`;
  `store::EventStore` stays direct, same reason.
- `message.rs` / `file.rs` — only the `build_message_text` / `build_agent_status`
  / `build_file_shared` call sites (and their content/id types where the
  builder signature needs them) route through the façade
  (`iroh_rooms::events` / `iroh_rooms::files`); the surrounding online engine,
  transport, and store imports stay direct `iroh-rooms-core` /
  `iroh-rooms-net` deps, as the spec explicitly scopes this narrower for the
  two online-heavy files.
- `agent.rs`, `audit.rs`, `cli.rs`, `display.rs`, `error.rs`, `join.rs`,
  `pipe.rs` — **not migrated** this issue (the online/experimental-heavy
  files; §5.4 calls this "recommended but optional"). Every symbol they use is
  already reachable through the façade per the tables above, so a future
  migration is a pure import swap with no design change.

`crates/iroh-rooms-cli/Cargo.toml` depends on `iroh-rooms` (with the
`experimental` feature, so the façade's re-exports stay a strict superset of
the CLI's needs) alongside the existing direct `iroh-rooms-core` /
`iroh-rooms-net` dependencies used by the unmigrated files.

## Method

This table was produced by auditing every production `use iroh_rooms_core::` /
`use iroh_rooms_net::` line in `crates/iroh-rooms-cli/src/*.rs`:

```console
grep -n "use iroh_rooms_core::\|use iroh_rooms_net::\|use iroh_rooms::" crates/iroh-rooms-cli/src/*.rs
```

against the façade's re-export lists in `crates/iroh-rooms/src/{identity,room,
events,files,pipes}.rs` and `crates/iroh-rooms/src/experimental/*.rs`.
