# CLI, SDK, and docs reachability inventory

Refs #343 (`RB-REL-007`). This is the completion-evidence inventory: every
shipped CLI command and flag, every public `iroh-rooms` façade export, and the
product capabilities advertised from the README, `docs/`, CLI help, and that
façade. A row is reachable when a production code path implements it and a
test exercises that path. Rows marked absent are names this tree must not
grow back without an inventory edit.

The fences below are compared to clap's command tree and to the façade
`pub use` lists by `crates/iroh-rooms-cli/src/reachability.rs` and
`crates/iroh-rooms/tests/reachability_inventory.rs`.

## Encryption-claims (OD-1)

v0.1 GA ships room content as plaintext, encrypted in transit only between
admitted peers (ADR-0014 OD-1, roadmap amendment RB-1.2). No CLI command, no
stable SDK item, and no production call site outside tests sets
`SyncConfig::encrypted_content_writes` (the field defaults to `false` in
`crates/iroh-rooms-core/src/sync/config.rs`). `room remove` and `room leave`
call `iroh_rooms::room::build_member_removed` and `build_member_left`.
`build_member_removed` hard-codes `rotation: None`. The rotation builder
`build_member_removed_with_rotation` is only
`iroh_rooms::experimental::rotation`.

Shipped rooms therefore have no content confidentiality beyond transport
encryption, and no forward secrecy after a member departs. Docs that said an
operator rotates, that a `rooms.db` read yields only ciphertext, or that a
deployment opts into post-departure secrecy were describing a path the
shipped CLI cannot reach. Those sentences are corrected in the release-notes
template, the threat model, and `docs/protocol.md`. Historical files under
`docs/releases/` are left unchanged.

## Shipped feature gates

| Gate | Shipped default | Where it is set | Test |
| --- | --- | --- | --- |
| `iroh-rooms-net/gossip_overlay` | off | not named by the CLI or SDK manifests | `shipped_manifests_do_not_enable_gossip_or_the_40_member_cap` |
| `iroh-rooms-core/large_rooms` (40-member cap) | off | enabled only by `gossip_overlay` | same test |
| `SyncConfig::encrypted_content_writes` | `false` | no `src/` assignment to `true` | `production_sources_do_not_enable_encrypted_content_writes` |

`MAX_ACTIVE_MEMBERS` stays 5 on that default. README and `room members
--status` advertise the 5-member ceiling; the 40-member cap is described as
compiled and not enabled.

## Member removal

`iroh-rooms room remove <ROOM_ID> <MEMBER_ID>` (admin only) and `iroh-rooms
room leave <ROOM_ID>` are in the clap tree and dispatched from
`crates/iroh-rooms-cli/src/cli.rs` to `crates/iroh-rooms-cli/src/member.rs`.
Removed members lose access to new messages, files, and pipes once peers see
the removal; they keep what they already had. The commands do not rotate
content keys.

| Surface | Test |
| --- | --- |
| CLI parse and refusal (`insufficient_role`, repeat leave, self-remove) | `crates/iroh-rooms-cli/tests/member_cli.rs` |
| Online push of the departure | `crates/iroh-rooms-cli/tests/two_peer_e2e.rs` (ignored online tier) |
| Removed device cannot pull new events | `crates/iroh-rooms-net/tests/manager_e2e.rs` |

## Product capabilities advertised as available

These are the capabilities the README "Current beta scope" and the guides
tell a user they can perform. Each one is a clap command in the fence below.

| Capability | Advertised in | Code path | Test |
| --- | --- | --- | --- |
| Local identity and device | README, `docs/getting-started.md` | `cli.rs` `Identity` → `identity.rs` | `identity_cli.rs` |
| Room create | README, getting-started | `room.rs` `create` | `room_cli.rs` |
| Key-bound invite | README, getting-started, `docs/community/demo-recipes.md` | `invite.rs` | `invite_cli.rs` |
| Join by ticket | README, getting-started | `join.rs` | `join_cli.rs` |
| Signed room message | README, getting-started | `message.rs` `send` | `message_cli.rs` |
| Room tail, including `--offline` | README, getting-started | `room.rs` `tail_offline`, `message.rs` `tail` | `tail_cli.rs` |
| Room members, including `--status` | README | `room.rs` `members`, `message.rs` `members_status` | `room_cli.rs`, `diagnostics_cli.rs` |
| Member remove and leave | README, getting-started, `SECURITY.md` | `member.rs` | `member_cli.rs` |
| Verified file share, list, fetch | README, getting-started | `file.rs` | `file_cli.rs` |
| Loopback live pipe expose, connect, close, list | README, `docs/live-pipe-preview.md` | `pipe.rs` | `pipe_cli.rs` |
| Agent invite and `agent status` | README, getting-started | `agent.rs` (invite delegates to `invite.rs`) | `agent_cli.rs`, `agent_invite_flow.rs` |
| Coded `error[<code>]:` / `next:` lines | README error-code tables | `error.rs` | `error_taxonomy.rs`, `docs_conformance.rs` |
| Verbose `diag:` network lines | README, getting-started | `message.rs` status and tail | `diagnostics_cli.rs`, `docs_conformance.rs` |
| Local `audit.ndjson` | README, `SECURITY.md` | `audit.rs` | `diagnostics_cli.rs` and the audit sink tests under `iroh-rooms-cli` |
| Stable offline SDK façade | README "Use the SDK", `docs/sdk-coverage.md` | `crates/iroh-rooms/src/{identity,room,events,files,pipes}.rs` | façade doctests (`cargo test -p iroh-rooms --doc`) and the stable fence below |
| Experimental online SDK | `crates/iroh-rooms` crate docs, feature `experimental` | `crates/iroh-rooms/src/experimental/` | `crates/iroh-rooms/tests/experimental_surface.rs` |

`PRD.v0.3.md` journeys are current requirements the README points at. Two
example forms did not parse: `room invite` without the required `--invitee`,
and `pipe expose --tcp localhost:3000` (`SocketAddr` rejects a hostname).
The examples now use `--invitee <identity-id>` and `--tcp 127.0.0.1:3000`,
which the CLI accepts. The commands themselves were already implemented.

## CLI command tree

`--help` and `--version` are clap builtins and are not listed. `--data-dir`
is global and listed once, on the root command. `hidden` marks clap
`hide = true` (the CI `--loopback` switch).

```text
reachability-cli
arg iroh-rooms agent invite <AGENT_ID>
arg iroh-rooms agent invite <ROOM_ID>
arg iroh-rooms agent status <ROOM_ID>
arg iroh-rooms agent status <STATUS>
arg iroh-rooms file fetch <FILE_ID>
arg iroh-rooms file fetch <ROOM_ID>
arg iroh-rooms file list <ROOM_ID>
arg iroh-rooms file share <PATH>
arg iroh-rooms file share <ROOM_ID>
arg iroh-rooms pipe close <PIPE_ID>
arg iroh-rooms pipe connect <PIPE_ID>
arg iroh-rooms pipe connect <ROOM_ID>
arg iroh-rooms pipe expose <ROOM_ID>
arg iroh-rooms pipe list <ROOM_ID>
arg iroh-rooms room create <NAME>
arg iroh-rooms room invite <ROOM_ID>
arg iroh-rooms room join <TICKET>
arg iroh-rooms room leave <ROOM_ID>
arg iroh-rooms room members <ROOM_ID>
arg iroh-rooms room remove <MEMBER>
arg iroh-rooms room remove <ROOM_ID>
arg iroh-rooms room send <MESSAGE>
arg iroh-rooms room send <ROOM_ID>
arg iroh-rooms room tail <ROOM_ID>
command iroh-rooms
command iroh-rooms agent
command iroh-rooms agent invite
command iroh-rooms agent status
command iroh-rooms file
command iroh-rooms file fetch
command iroh-rooms file list
command iroh-rooms file share
command iroh-rooms identity
command iroh-rooms identity create
command iroh-rooms identity show
command iroh-rooms pipe
command iroh-rooms pipe close
command iroh-rooms pipe connect
command iroh-rooms pipe expose
command iroh-rooms pipe list
command iroh-rooms room
command iroh-rooms room create
command iroh-rooms room invite
command iroh-rooms room join
command iroh-rooms room leave
command iroh-rooms room members
command iroh-rooms room remove
command iroh-rooms room send
command iroh-rooms room tail
flag iroh-rooms --data-dir
flag iroh-rooms agent invite --expires
flag iroh-rooms agent status --artifact
flag iroh-rooms agent status --loopback hidden
flag iroh-rooms agent status --message
flag iroh-rooms agent status --peer
flag iroh-rooms agent status --progress
flag iroh-rooms agent status --timeout
flag iroh-rooms file fetch --loopback hidden
flag iroh-rooms file fetch --out
flag iroh-rooms file fetch --peer
flag iroh-rooms file fetch --timeout
flag iroh-rooms file list --json
flag iroh-rooms file share --mime
flag iroh-rooms file share --name
flag iroh-rooms identity create --force
flag iroh-rooms identity create --name
flag iroh-rooms identity show --json
flag iroh-rooms pipe close --loopback hidden
flag iroh-rooms pipe close --peer
flag iroh-rooms pipe close --room
flag iroh-rooms pipe connect --local
flag iroh-rooms pipe connect --loopback hidden
flag iroh-rooms pipe connect --peer
flag iroh-rooms pipe expose --allow
flag iroh-rooms pipe expose --expires
flag iroh-rooms pipe expose --label
flag iroh-rooms pipe expose --loopback hidden
flag iroh-rooms pipe expose --peer
flag iroh-rooms pipe expose --tcp
flag iroh-rooms pipe expose --verbose
flag iroh-rooms room invite --expires
flag iroh-rooms room invite --invitee
flag iroh-rooms room invite --role
flag iroh-rooms room join --display-name
flag iroh-rooms room join --loopback hidden
flag iroh-rooms room join --peer
flag iroh-rooms room join --timeout
flag iroh-rooms room leave --loopback hidden
flag iroh-rooms room leave --peer
flag iroh-rooms room leave --reason
flag iroh-rooms room leave --timeout
flag iroh-rooms room members --json
flag iroh-rooms room members --loopback hidden
flag iroh-rooms room members --peer
flag iroh-rooms room members --status
flag iroh-rooms room members --timeout
flag iroh-rooms room members --verbose
flag iroh-rooms room remove --loopback hidden
flag iroh-rooms room remove --peer
flag iroh-rooms room remove --reason
flag iroh-rooms room remove --timeout
flag iroh-rooms room send --format
flag iroh-rooms room send --loopback hidden
flag iroh-rooms room send --peer
flag iroh-rooms room send --reply-to
flag iroh-rooms room send --timeout
flag iroh-rooms room tail --accept-joins
flag iroh-rooms room tail --json
flag iroh-rooms room tail --limit
flag iroh-rooms room tail --loopback hidden
flag iroh-rooms room tail --offline
flag iroh-rooms room tail --peer
flag iroh-rooms room tail --verbose
```

## Commands that stay absent

These names appear in future-work notes (`room list`, `room export`, `file
unpin`, a smoke-check is not a command) or were implied by help text
(`agent drive`) or by encryption wording (`room rotate`, `room encrypt`).
None is a clap command. `PRODUCTION-READINESS.md` still lists `room list` as
a P2 gate to build, not as a command that exists.

```text
reachability-cli-removed
iroh-rooms agent drive
iroh-rooms file serve
iroh-rooms file unpin
iroh-rooms identity export
iroh-rooms room encrypt
iroh-rooms room export
iroh-rooms room list
iroh-rooms room rotate
```

## Stable SDK surface

Default features. Every `module` and re-exported name below is a `pub mod` or
`pub use` in `crates/iroh-rooms/src` outside `experimental/`. The rotation
builder is not in this list.

```text
reachability-sdk-stable
iroh_rooms::events::Content
iroh_rooms::events::EventId
iroh_rooms::events::EventType
iroh_rooms::events::Flag
iroh_rooms::events::HashRef
iroh_rooms::events::MembershipOracle
iroh_rooms::events::PROTOCOL_VERSION
iroh_rooms::events::RejectReason
iroh_rooms::events::RoomId
iroh_rooms::events::SignedEvent
iroh_rooms::events::ValidatedEvent
iroh_rooms::events::ValidationContext
iroh_rooms::events::WireEvent
iroh_rooms::events::build_agent_status
iroh_rooms::events::build_message_text
iroh_rooms::events::capability_hash
iroh_rooms::events::constants::*
iroh_rooms::events::validate_wire_bytes
iroh_rooms::events::validate_with_membership
iroh_rooms::files::FileShared
iroh_rooms::files::HashRef
iroh_rooms::files::build_file_shared
iroh_rooms::identity::DeviceBinding
iroh_rooms::identity::DeviceKey
iroh_rooms::identity::IdentityKey
iroh_rooms::identity::Signature
iroh_rooms::identity::SigningKey
iroh_rooms::pipes::PipeClosed
iroh_rooms::pipes::PipeOpened
iroh_rooms::pipes::build_pipe_closed
iroh_rooms::pipes::build_pipe_opened
iroh_rooms::prelude::EventId
iroh_rooms::prelude::IdentityKey
iroh_rooms::prelude::RejectReason
iroh_rooms::prelude::Role
iroh_rooms::prelude::RoomId
iroh_rooms::prelude::RoomInviteTicket
iroh_rooms::prelude::SigningKey
iroh_rooms::prelude::Status
iroh_rooms::prelude::WireEvent
iroh_rooms::prelude::build_member_invited
iroh_rooms::prelude::build_member_joined
iroh_rooms::prelude::build_message_text
iroh_rooms::prelude::build_room_created
iroh_rooms::room::AncestorView
iroh_rooms::room::BlobDecision
iroh_rooms::room::DenyReason
iroh_rooms::room::Ingest
iroh_rooms::room::Member
iroh_rooms::room::MembershipSnapshot
iroh_rooms::room::PipeDecision
iroh_rooms::room::Role
iroh_rooms::room::RoomId
iroh_rooms::room::RoomInviteTicket
iroh_rooms::room::RoomMembership
iroh_rooms::room::Status
iroh_rooms::room::TicketError
iroh_rooms::room::blob_serve_allowed
iroh_rooms::room::build_member_invited
iroh_rooms::room::build_member_joined
iroh_rooms::room::build_member_left
iroh_rooms::room::build_member_removed
iroh_rooms::room::build_room_created
iroh_rooms::room::derive_room_id
iroh_rooms::room::pipe_connect_allowed
module iroh_rooms::events
module iroh_rooms::events::constants
module iroh_rooms::files
module iroh_rooms::identity
module iroh_rooms::pipes
module iroh_rooms::prelude
module iroh_rooms::room
```

## Experimental SDK surface

`experimental` cargo feature only. Reachable for an embedder who opts into
that feature, including the rotation builder. Not reachable from the shipped
CLI, which never calls it.

```text
reachability-sdk-experimental
iroh_rooms::experimental::blob::BlobAclView
iroh_rooms::experimental::blob::BlobError
iroh_rooms::experimental::blob::BlobImport
iroh_rooms::experimental::blob::BlobStore
iroh_rooms::experimental::blob::EndpointId
iroh_rooms::experimental::blob::FetchOutcome
iroh_rooms::experimental::pipe_runtime::EndpointId
iroh_rooms::experimental::pipe_runtime::PIPE_ALPN
iroh_rooms::experimental::pipe_runtime::PipeAuditSink
iroh_rooms::experimental::pipe_runtime::PipeDenyCause
iroh_rooms::experimental::pipe_runtime::PipeError
iroh_rooms::experimental::pipe_runtime::PipeForwarder
iroh_rooms::experimental::pipe_runtime::PipeOutcome
iroh_rooms::experimental::pipe_runtime::PipeRegistry
iroh_rooms::experimental::pipe_runtime::PipeSessionInfo
iroh_rooms::experimental::pipe_runtime::TracingPipeAudit
iroh_rooms::experimental::pipe_runtime::is_loopback_target
iroh_rooms::experimental::pipe_runtime::new_pipe_id
iroh_rooms::experimental::rotation::DistributionError
iroh_rooms::experimental::rotation::build_member_removed_with_rotation
iroh_rooms::experimental::session::Admission
iroh_rooms::experimental::session::AdmissionDecision
iroh_rooms::experimental::session::AdmissionView
iroh_rooms::experimental::session::AllowlistAdmission
iroh_rooms::experimental::session::AuditSink
iroh_rooms::experimental::session::BlobDenyCause
iroh_rooms::experimental::session::BlobServeConfig
iroh_rooms::experimental::session::BootstrapProof
iroh_rooms::experimental::session::ConnEvent
iroh_rooms::experimental::session::DEFAULT_TICK
iroh_rooms::experimental::session::EVENT_ALPN
iroh_rooms::experimental::session::Endpoint
iroh_rooms::experimental::session::EndpointAddr
iroh_rooms::experimental::session::EndpointId
iroh_rooms::experimental::session::EventProtocolHandler
iroh_rooms::experimental::session::Inbound
iroh_rooms::experimental::session::JoinBootstrapAdmission
iroh_rooms::experimental::session::NetConfig
iroh_rooms::experimental::session::NetMode
iroh_rooms::experimental::session::NetTransport
iroh_rooms::experimental::session::Node
iroh_rooms::experimental::session::OfflineReason
iroh_rooms::experimental::session::PeerConnState
iroh_rooms::experimental::session::PeerEntry
iroh_rooms::experimental::session::PeerManager
iroh_rooms::experimental::session::PeerTable
iroh_rooms::experimental::session::RELAY_ONLY_TEST_BUILD
iroh_rooms::experimental::session::RejectCause
iroh_rooms::experimental::session::SecretKey
iroh_rooms::experimental::session::Shared
iroh_rooms::experimental::session::SnapshotAdmission
iroh_rooms::experimental::session::TracingAudit
iroh_rooms::experimental::store::EventStore
iroh_rooms::experimental::store::InsertOutcome
iroh_rooms::experimental::store::InsertStats
iroh_rooms::experimental::store::ParkedRow
iroh_rooms::experimental::store::StoreError
iroh_rooms::experimental::store::StoreOptions
iroh_rooms::experimental::store::StoredEvent
iroh_rooms::experimental::store::SyncStateRow
iroh_rooms::experimental::store::TrustRow
iroh_rooms::experimental::sync::Completeness
iroh_rooms::experimental::sync::MessageError
iroh_rooms::experimental::sync::Outgoing
iroh_rooms::experimental::sync::PeerId
iroh_rooms::experimental::sync::Severity
iroh_rooms::experimental::sync::SyncConfig
iroh_rooms::experimental::sync::SyncCounters
iroh_rooms::experimental::sync::SyncDigest
iroh_rooms::experimental::sync::SyncEngine
iroh_rooms::experimental::sync::SyncError
iroh_rooms::experimental::sync::SyncMessage
iroh_rooms::experimental::sync::SyncTransport
iroh_rooms::experimental::sync::TrustDecision
iroh_rooms::experimental::sync::Window
iroh_rooms::experimental::sync::WireBytes
module iroh_rooms::experimental
module iroh_rooms::experimental::blob
module iroh_rooms::experimental::pipe_runtime
module iroh_rooms::experimental::rotation
module iroh_rooms::experimental::session
module iroh_rooms::experimental::store
module iroh_rooms::experimental::sync
```

## Removals in this change

Advertisements of a capability the shipped surface cannot reach, removed or
rewritten. No reachable command, flag, or stable API item was deleted.

- CLI help for `agent` no longer says it will "drive" agent participants.
  The subcommands are `invite` and `status`.
- `docs/operations/production-beta-release-notes-template.md` no longer tells
  a release author to describe operator key rotation, a room opt-in, or a
  `rooms.db` that holds ciphertext plus keys for ordinary CLI rooms.
- `docs/security/threat-model.md` no longer says the CLI has no
  member-removal command, that production code authors no removal, or that a
  read of `rooms.db` yields only ciphertext. T29 no longer says a departed
  device keeps decrypting shipped-room content until an operator rotates.
- `docs/protocol.md` limitations no longer say content-key rotation buys
  forward secrecy in a deployment that opts in.
- `docs/sdk-coverage.md` no longer claims every CLI import has a façade path.
- `PRD.v0.3.md` invite and pipe examples now match forms clap accepts.

## Needs a decision

Left in place because removing them would be a breaking public API change, or
because the file is a preserved record:

- `SyncConfig::encrypted_content_writes` and
  `iroh_rooms::experimental::rotation::{build_member_removed_with_rotation, DistributionError}`
  stay public on the experimental tier. An embedder can call them. The
  shipped CLI does not.
- Stable `iroh_rooms::events::constants` still re-exports the encryption
  size bounds (`MAX_ENCRYPTED_*`, `ENCRYPTED_AAD_CONTEXT`). They are the
  validation limits for envelopes the engine can ingest. Removing the glob
  would break stable API users.
- `ACTIVE_MEMBER_WARNING_THRESHOLD` and `active_member_warning_crossed` are
  used from `iroh-rooms-core` and are not on the façade. Re-exporting them
  would add stable symbols; deleting the call sites would change the
  near-cap warning, which is reachable.
- `docs/releases/*` still contain older sentences (rc.5's "content-key
  rotation available (opt-in)", and "the CLI has no member-removal"). Those
  records are not rewritten. rc.6 notes already state the later decision.
- `PRD.md` (historical v0.2) and `specs/getting-started-demo-script.md` still
  show `--tcp localhost:3000`. The current guide and `PRD.v0.3.md` do not.
- `PRD.v0.3.md` section 16's command list does not yet include `room remove`
  or `room leave`, which do ship. Adding them is a requirements edit, not
  the deletion of an unreachable command.
- Threat-model T21 still describes the supported build as unsigned rc.5.
  That is release-status drift, not a capability to delete here.
- `docs/security/v0.1-self-review.md` checkboxes stay open. Linking this
  file from issue #343 would be an issue comment, which this change does not
  post.
