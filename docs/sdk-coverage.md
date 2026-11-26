# SDK façade coverage (RB-SDK-001)

The supported Rust entry point is the `iroh-rooms` crate. It re-exports the
same types `iroh-rooms-core` and `iroh-rooms-net` already expose; it does not
wrap them. A default-features build can name only the stable modules
(`identity`, `room`, `events`, `files`, `pipes`, `prelude`). The online
runtime is `iroh_rooms::experimental`, behind the `experimental` feature.

`crates/iroh-rooms-cli` is the reference consumer. Its `src/` and `tests/`
name those types only through `iroh_rooms`. The manifest does not depend on
`iroh-rooms-core` or `iroh-rooms-net`.
`crates/iroh-rooms-cli/tests/no_direct_internal_deps.rs` fails if either
direct path comes back. `tests/no_direct_iroh_dep.rs` is the sibling guard
for a direct `iroh` dependency.

Stable-surface tests live in `crates/iroh-rooms/tests/stable_surface.rs`
(default features, façade paths only). Experimental paths are locked in
`crates/iroh-rooms/tests/experimental_surface.rs`.

Rows below that are not implemented have no SDK symbol. This pass does not
delete those capabilities from core, net, or the docs. Removal of
advertised-but-unreachable behavior belongs to RB-REL-007.

## Dependency rows

Each row is a dependency of RB-SDK-001. "SDK-visible surface" is the façade
path a consumer can name today. "None" means the row's deliverable is not
implemented, so there is nothing further to re-export.

| Row | SDK-visible surface |
|---|---|
| RB-ENTRY-001 | None. Moderation envelope/body identifier consistency is not on the v1 façade. v2 code stays in `iroh-rooms-v2-core` and is not re-exported. |
| RB-ENTRY-002 | None for the new `InvalidContent` classification. Existing rejects are `events::RejectReason`. |
| RB-ENTRY-003 | None. The row is a coverage-scanner change, not a runtime type. |
| RB-ENTRY-004 | None. Frozen member-Merkle fixtures are not an SDK type. |
| RB-ENTRY-005 | None. The `activi` proof erratum is a fixture/docs successor, not an SDK type. |
| RB-ENTRY-006 | None. The audit-claim erratum does not add a symbol. |
| RB-ENTRY-007 | None. Successor v2 domains are not re-exported. |
| RB-ADM-001 | `room::RoomMembership::membership_projection_generation` is the only cache key. The engine cache itself is inside `experimental::sync::SyncEngine`. |
| RB-ADM-002 | `experimental::sync::{Completeness, Severity, TrustDecision}`. `Completeness::AdminForkDetected` is the fork verdict. |
| RB-ADM-003 | Same sync verdicts, plus `experimental::session::AuditSink` for CLI audit hooks. There is no separate operator-divergence command or SDK method yet. |
| RB-ADM-004 | `experimental::sync::Completeness` (partition/rejoin detection the engine already computes). No extra façade method. |
| RB-ADM-005 | `experimental::rotation::build_member_removed_with_rotation` and `DistributionError`. Conflict resolution across epochs is not a separate SDK method. v0.1 GA removal uses `room::build_member_removed` and does not rotate. |
| RB-SEC-001 | None for revocation. The ticket codec is `room::{RoomInviteTicket, TicketError}`. |
| RB-SEC-002 | Shipped removal is `room::build_member_removed` (plaintext, no rotation). The rotation builder is `experimental::rotation::build_member_removed_with_rotation`. The CLI does not call it. |
| RB-SEC-003 | None for device verification. The binding certificate is `identity::DeviceBinding`. |
| RB-SEC-004 | None. `experimental::store::EventStore` is the local log; it is not an at-rest-encryption API. |
| RB-SEC-005 | None. No recovery-phrase type. |
| RB-SEC-006 | None. No backup/restore type on the façade. |
| RB-SEC-007 | `room::Role` (`Admin`, `Member`, `Agent`) only. No separate agent trust-level type. |
| RB-SEC-008 | Per-pipe allowlists: `pipes::build_pipe_opened` and `room::pipe_connect_allowed`. No room-level pipe-policy object. |
| RB-SEC-009 | None. `experimental::blob::BlobStore` stores blobs; it does not encrypt them at rest. |
| RB-SEC-011 | The CLI display test names `experimental::rotation::EncryptedContent`. The seal/open builders (`build_content_encrypted`, `open_encrypted_content`, `SealError`, `UnreadableReason`) stay in core: the CLI does not name them. v0.1 GA ships plaintext content, encrypted in transit only. |
| RB-SEC-012 | `experimental::sync::SyncMessage` includes `WantKeyHistory` and `KeyHistory`. `KeyHistoryChunk` is not re-exported; the CLI does not name it. No CLI paging command. |
| RB-SEC-013 | The CLI matches the `Content::MemberKeyDistribution` variant and never names the `MemberKeyDistribution` or `WrappedKeyEntry` types. Those payload types are re-exported at `experimental::rotation` for a façade consumer. No new provenance-id type. |
| RB-SEC-014 | None beyond the sync engine. No poison-state SDK type. |
| RB-SEC-015 | None. No blocklist or moderation product type. |
| RB-SEC-016 | `experimental::session::AuditSink` (and `TracingAudit`). No retention/rotation API. The CLI's `audit.ndjson` writer is CLI-local. |
| RB-SEC-017 | None. No tamper-evident audit chain type. |
| RB-SEC-018 | None. No audit-export type. |
| RB-NET-001 | None on the shipping façade. `actor.subscribe` bounding is a gossip-overlay concern. `experimental` does not enable `gossip_overlay`. |
| RB-NET-002 | None specific. Mesh spawn/retry audit events are not a façade vocabulary. `experimental::session::AuditSink` is the generic hook. |
| RB-NET-003 | None. No zero-neighbor resubscribe API. |
| RB-NET-004 | None. The measurement harness is not an SDK type. |
| RB-NET-005 | None. Real-network runs are operational evidence, not an SDK type. |
| RB-NET-006 | None. The shipping façade does not forward `iroh-rooms-net`'s `gossip_overlay` feature, and `GOSSIP_ALPN` / `GOSSIP_BOOTSTRAP_SEEDS` are not re-exported. |
| RB-NET-007 | None. Gate A measurements are not an SDK type. |
| RB-NET-008 | None. Cellular relay measurements are not an SDK type. |
| RB-UP-101 | None. No `room list` type or method. |
| RB-UP-102 | None for an all-event NDJSON schema type. A consumer reads `experimental::store::{EventStore, StoredEvent}` and `events::SignedEvent`. |
| RB-UP-103 | `events::Content` and the payload structs in the symbol table below. Rendering stays in the CLI. |
| RB-UP-104 | `experimental::session::Node::pipe_session_info` returns `experimental::pipe_runtime::PipeSessionInfo`. No `pipe list --json` schema type. |
| RB-UP-105 | `experimental::session::{PathType, PeerConnState, PeerEntry, OfflineReason, ConnEvent}`. `Node::peer_paths` is what the CLI's `diag:` block reads. No stable diagnostics-JSON schema type. |
| RB-UP-106 | `experimental::sync::SyncCounters` and `experimental::blob::FetchOutcome`. `experimental::pipe_runtime::PIPE_MAX_CONCURRENT_FORWARDS` is the connector budget, not a byte counter. |
| RB-CLI-001 | None. No smoke-check type. |
| RB-PIPE-001 | None. No terminal-sharing type. TCP pipes are `pipes` plus `experimental::pipe_runtime`. |
| RB-PIPE-002 | None. No Unix-socket forwarding type. |
| RB-PIPE-003 | None for multiplexed services. The single-pipe budget is `experimental::pipe_runtime::PIPE_MAX_CONCURRENT_FORWARDS`. |
| RB-PRD3-001 | None. No `task.created` type. |
| RB-PRD3-002 | None. No `task.updated` type. |
| RB-PRD3-003 | None. No `agent.output` type. Agent presence today is `room::Role::Agent` and `events::build_agent_status`. |
| RB-PRD3-004 | None. No `agent.error` type. |
| RB-PRD3-005 | None. No `agent.artifact.shared` type. |
| RB-PRD3-006 | None. No `agent.review.requested` type. |
| RB-PRD3-007 | None. No live-agent-log stream type. Authorized TCP pipes remain `pipes` and `experimental::pipe_runtime`. |
| RB-PRD3-010 | None. No room-export type. |

RB-PRD3-005, RB-PRD3-006, and RB-PRD3-007 are included because the dependency
expression is `RB-PRD3-001..007`. Their deliverables (`agent.artifact.shared`,
`agent.review.requested`, and live agent logs) are not façade types.

## Stable symbols

| Core symbol | Façade path |
|---|---|
| `event::keys::{IdentityKey, DeviceKey, SigningKey, Signature}` | `identity::{IdentityKey, DeviceKey, SigningKey, Signature}` |
| `event::binding::DeviceBinding` | `identity::DeviceBinding` |
| `event::ids::RoomId` | `room::RoomId` and `events::RoomId` |
| `event::ids::EventId` | `events::EventId` |
| `event::ids::HashRef` | `files::HashRef` and `events::HashRef` |
| `event::content::{Content, EventType, capability_hash}` | `events::{Content, EventType, capability_hash}` |
| `event::content::{MessageText, AgentStatus}` | `events::{MessageText, AgentStatus}` |
| `event::content::{RoomCreated, MemberInvited, MemberJoined, MemberLeft, MemberRemoved}` | `room::` of the same names |
| `event::content::FileShared` | `files::FileShared` |
| `event::content::{PipeOpened, PipeClosed}` | `pipes::{PipeOpened, PipeClosed}` |
| `event::signed::SignedEvent` | `events::SignedEvent` |
| `event::signed::derive_room_id` | `room::derive_room_id` |
| `event::validate::{validate_wire_bytes, validate_with_membership, ValidatedEvent, ValidationContext}` | `events::` of the same names |
| `event::reject::{Flag, MembershipOracle, RejectReason}` | `events::` of the same names |
| `event::wire::WireEvent` | `events::WireEvent` |
| `event::{build_message_text, build_agent_status}` | `events::` of the same names |
| `event::build_file_shared` | `files::build_file_shared` |
| `event::{build_pipe_opened, build_pipe_closed}` | `pipes::` of the same names |
| `event::{build_room_created, build_member_invited, build_member_joined, build_member_left, build_member_removed}` | `room::` of the same names |
| `event::constants::*` | `events::constants::*` |
| `PROTOCOL_VERSION` | `events::PROTOCOL_VERSION` |
| `membership::{RoomMembership, MembershipSnapshot, Member, Ingest, AncestorView, Role, Status, BlobDecision, PipeDecision, DenyReason, blob_serve_allowed, pipe_connect_allowed}` | `room::` of the same names |
| `ticket::{RoomInviteTicket, TicketError}` | `room::` of the same names |

`MAX_ACTIVE_MEMBERS`, `ACTIVE_MEMBER_WARNING_THRESHOLD`, and
`active_member_warning_crossed` are not re-exported. The cap is 5 without
`large_rooms` and 40 with it, so a stable constant would lie. The CLI reads
`MembershipSnapshot::active_member_limit()` and derives the near-cap warning
as one below that limit. `iroh-rooms-net` still calls the core function
internally.
[`docs/reachability-inventory.md`](reachability-inventory.md) records those
symbols as core-only. Re-exporting them would add stable public symbols, and
the reachability pass does not add that re-export. `MembershipSnapshot` on
`iroh_rooms::room` also exposes `active_member_headroom`, so the headroom
surface is reachable. The threshold constant and the crossing detector are
not a CLI feature a user can call.

## Experimental symbols

| Net or core symbol | Façade path |
|---|---|
| `store::{EventStore, StoredEvent, InsertOutcome, InsertStats, ParkedRow, StoreError, StoreOptions, SyncStateRow, TrustRow}` | `experimental::store::` |
| `sync::{SyncConfig, SyncEngine, SyncMessage, SyncError, SyncTransport, Completeness, Severity, TrustDecision, SyncCounters, SyncDigest, ...}` | `experimental::sync::` |
| `net` session/admission/transport types the CLI names, including `Node`, `NetConfig`, `PathType`, `BootstrapProof`, `AuditSink` | `experimental::session::` |
| `iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey}` | `experimental::session::` (and `EndpointId` again on `blob` and `pipe_runtime`) |
| `net::{BlobStore, BlobError, BlobImport, BlobAclView, FetchOutcome}` | `experimental::blob::` |
| `net` pipe runtime, including `is_loopback_target` and `PIPE_MAX_CONCURRENT_FORWARDS` | `experimental::pipe_runtime::` |
| `event::content::{EncryptedContent, MemberKeyDistribution, WrappedKeyEntry}` | `experimental::rotation::` of the same names |
| `event::{build_member_removed_with_rotation, DistributionError}` | `experimental::rotation::` |

`PathType` is the classification `Node::peer_paths` returns. The CLI's
diagnostics block matches it. `classify_remote_info` is not re-exported: its
argument is `iroh::endpoint::RemoteInfo`, and the CLI does not call it.

`RoomKey` is not re-exported. The experimental encryption builders take
`&iroh_rooms_crypto::RoomKey`, the same signature core publishes.

## Intentionally not re-exported

These stay public on the internal crate. They were not removed.

- `MAX_ACTIVE_MEMBERS`, `ACTIVE_MEMBER_WARNING_THRESHOLD`, and `active_member_warning_crossed`. The CLI uses `MembershipSnapshot::active_member_limit()` instead. Net still calls the core function.
- Encryption builders the CLI does not name: `build_content_encrypted`, `open_encrypted_content`, `build_key_distribution_content`, `build_member_key_distribution`, `SealError`, `UnreadableReason`.
- `sync::message::KeyHistoryChunk` and `sync::MAX_FRAME_BYTES`. `SyncMessage` is re-exported; the CLI does not name the chunk type or the frame cap.
- `event::cbor` — raw deterministic CBOR. Consumers use `WireEvent` and `validate_wire_bytes`.
- `sync::sim` — in-memory test harness.
- Gossip-only net items (`GOSSIP_ALPN`, `GOSSIP_BOOTSTRAP_SEEDS`) and the `gossip_overlay` feature. The shipping CLI and the façade's `experimental` feature keep full-mesh and the cap of 5.
- Transport internals the CLI does not name: `FrameError`, `InboundReceiver`, `OutboundQueue`, `MAX_CONCURRENT_BIDI_STREAMS`.
- `iroh-rooms-v2-core` — not part of the v1 façade.

## Method

Regenerate the CLI half of this audit when imports change:

```console
grep -n "iroh_rooms::" crates/iroh-rooms-cli/src/*.rs crates/iroh-rooms-cli/tests/*.rs
```

That grep must not show `iroh_rooms_core::` or `iroh_rooms_net::` outside
`tests/no_direct_iroh_dep.rs`, which documents the bare-`iroh` scanner.
The guard test enforces the same rule.
