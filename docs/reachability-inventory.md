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
admitted peers (ADR-0014 OD-1, roadmap amendment RB-1.2). The encryption
machinery has three reachability states in today's tree. Cites below were
checked against that tree. Inbound key adoption is not gated off.

### (a) Unreachable in the shipped binary

Sealing and rotation authoring. `SyncConfig::default` sets
`encrypted_content_writes: false`
(`crates/iroh-rooms-core/src/sync/config.rs:209`). The writer gate refuses a
locally authored `content.encrypted` publish while that flag is false
(`crates/iroh-rooms-core/src/sync/engine.rs:838`). No CLI or
`iroh-rooms-net` production source sets the flag or constructs a sealed
content envelope. `room remove` and `room leave` call `build_member_removed`
and `build_member_left`. `build_member_removed` sets `rotation: None`
(`crates/iroh-rooms-core/src/event/removed.rs:60`).
`build_member_removed_with_rotation` is not called from the shipped binary.

### (b) Reachable from peer input in the shipped binary

Live engines open with the local device seed. `room tail` does so at
`crates/iroh-rooms-cli/src/message.rs:502-506`
(`SyncEngine::open_with_local_device` with `SyncConfig::default()` and
`Some(secret.device_seed())`). The same opener is used by `message.rs:639`,
`message.rs:874`, `message.rs:957`, `message.rs:1061`, `file.rs:517`,
`join.rs:160`, `pipe.rs:151`, `pipe.rs:285`, and `pipe.rs:447`.

On insert, `apply_insert_outcome` calls `adopt_key_from_event`
(`engine.rs:1790`). That function adopts a `MemberKeyDistribution` or the
`rotation` payload on `member.removed` (`engine.rs:3098-3105`), calls
`unwrap_room_key` (`engine.rs:3220`), and on a fresh adoption persists the
key (`engine.rs:3283-3285`). An inbound `WantKeyHistory` is served
(`engine.rs:933-934`). Any current room member (a device admission accepts;
removed members and provisional invitees are refused) running a non-shipped
client can send KeyHistory chunks that the shipped engine adopts without
binding them to an admin-signed distribution (engine.rs:936-938,
:3291-3299), populating room_keys persistently. DAG key distributions are
admin-gated (fold.rs:390). See #429. Plaintext
confidentiality holds for v0.1: the shipped CLI seals nothing and never
decrypts for display.

Content decryption of pipe and file envelopes is on that same path.
`encrypted_bodies` calls `read_content` (`engine.rs:1256`). `pipe_opened`
(`engine.rs:1304`), `pipe_is_closed` (`engine.rs:1355`), and
`file_shared_hashes` (`engine.rs:1394`) call `encrypted_bodies`. The node
pump calls `file_shared_hashes`
(`crates/iroh-rooms-net/src/node.rs:341`). `pipe connect` calls
`node.pipe_opened` (`crates/iroh-rooms-cli/src/pipe.rs:319`). A
peer-supplied `content.encrypted` envelope of those inner types is opened
during those lookups. The decrypted `pipe.opened`, `pipe.closed`, and
`file.shared` bodies drive pipe-authorization and blob-serving decisions,
not just display. An unreadable remote encrypted pipe.closed makes
pipe_is_closed close every pipe (engine.rs:1329-1366, pipe/gate.rs:52).
`readable_content` itself has no CLI caller; message display does not use it.

### (c) Reachable for experimental SDK embedders

`SyncConfig::encrypted_content_writes` and
`iroh_rooms::experimental::rotation` (`build_member_removed_with_rotation`,
`DistributionError`) are reachable for an embedder who opts into the
`experimental` feature. The paragraphs kept below state that the shipped CLI
does not call them.

Docs that said an operator rotates, that every `rooms.db` read yields only
ciphertext, or that a deployment opts into post-departure secrecy described
authoring the shipped CLI does not do. Inbound adoption and the pipe/file
decryption walks above still run. Those sentences are corrected in the
release-notes template, the threat model, and `docs/protocol.md`. Historical
files under `docs/releases/` are left unchanged.

## Shipped feature gates

| Gate | Shipped default | Where it is set | Test |
| --- | --- | --- | --- |
| `iroh-rooms-net/gossip_overlay` | off | not named by the CLI or SDK manifests | `shipped_manifests_do_not_enable_gossip_or_the_40_member_cap` |
| `iroh-rooms-core/large_rooms` (40-member cap) | off | enabled only by `gossip_overlay` | same test |
| `SyncConfig::encrypted_content_writes` | `false` (`config.rs:209`) | the field name appears in `src/` only in `sync/config.rs` and `sync/engine.rs` | `encrypted_content_writes_name_is_confined_to_config_and_engine` |

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
iroh_rooms::events::AgentStatus
iroh_rooms::events::Content
iroh_rooms::events::EventId
iroh_rooms::events::EventType
iroh_rooms::events::Flag
iroh_rooms::events::HashRef
iroh_rooms::events::MembershipOracle
iroh_rooms::events::MessageText
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
iroh_rooms::events::constants::BIND_CONTEXT
iroh_rooms::events::constants::CLOCK_SKEW_FUTURE_MS
iroh_rooms::events::constants::DIGEST_LEN
iroh_rooms::events::constants::ENCRYPTED_AAD_CONTEXT
iroh_rooms::events::constants::ENCRYPTED_NONCE_LEN
iroh_rooms::events::constants::ENCRYPTED_SUITE_V1
iroh_rooms::events::constants::ENCRYPTED_TAG_LEN
iroh_rooms::events::constants::EVENT_CONTEXT
iroh_rooms::events::constants::INVITE_CONTEXT
iroh_rooms::events::constants::MAX_ARTIFACT_REFS
iroh_rooms::events::constants::MAX_ENCRYPTED_AGENT_STATUS_PLAINTEXT
iroh_rooms::events::constants::MAX_ENCRYPTED_FILE_SHARED_PLAINTEXT
iroh_rooms::events::constants::MAX_ENCRYPTED_MESSAGE_TEXT_PLAINTEXT
iroh_rooms::events::constants::MAX_ENCRYPTED_PIPE_CLOSED_PLAINTEXT
iroh_rooms::events::constants::MAX_ENCRYPTED_PIPE_OPENED_PLAINTEXT
iroh_rooms::events::constants::MAX_FILE_NAME_BYTES
iroh_rooms::events::constants::MAX_FILE_PROVIDERS
iroh_rooms::events::constants::MAX_MESSAGE_BODY_BYTES
iroh_rooms::events::constants::MAX_MIME_TYPE_BYTES
iroh_rooms::events::constants::MAX_PREV_EVENTS
iroh_rooms::events::constants::MAX_SHARED_FILE_BYTES
iroh_rooms::events::constants::MAX_STATUS_LABEL_BYTES
iroh_rooms::events::constants::MAX_STATUS_MESSAGE_BYTES
iroh_rooms::events::constants::PUBLIC_KEY_LEN
iroh_rooms::events::constants::ROOMID_CONTEXT
iroh_rooms::events::constants::SCHEMA_VERSION
iroh_rooms::events::constants::SHORT_ID_LEN
iroh_rooms::events::constants::SIGNATURE_LEN
iroh_rooms::events::constants::WIRE_VERSION
iroh_rooms::events::constants::WRAPPED_KEY_CIPHERTEXT_LEN
iroh_rooms::events::constants::WRAPPED_KEY_NONCE_LEN
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
iroh_rooms::room::MemberInvited
iroh_rooms::room::MemberJoined
iroh_rooms::room::MemberLeft
iroh_rooms::room::MemberRemoved
iroh_rooms::room::MembershipSnapshot
iroh_rooms::room::PipeDecision
iroh_rooms::room::Role
iroh_rooms::room::RoomCreated
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
iroh_rooms::experimental::pipe_runtime::PIPE_MAX_CONCURRENT_FORWARDS
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
iroh_rooms::experimental::rotation::EncryptedContent
iroh_rooms::experimental::rotation::MemberKeyDistribution
iroh_rooms::experimental::rotation::WrappedKeyEntry
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
iroh_rooms::experimental::session::PathType
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
