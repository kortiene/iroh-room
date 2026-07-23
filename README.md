# Iroh Rooms

Iroh Rooms is a local-first collaboration runtime for small trusted groups. It lets people and agents create a private room, exchange signed messages, share verified files, expose a loopback-only live TCP pipe, and keep the room state on their own machines instead of a central application server.

The current release is `v0.1.0-rc.3`, a controlled Production Beta for technical builders. It is ready for narrow, supervised use in private CLI-first rooms. It is not a general public launch, hosted chat service, compliance product, or polished GUI app.

## What you can do with it

Use Iroh Rooms when you want a private local workspace with peers you already trust:

- **Create a room without accounts**: each participant has a local identity and device key
- **Invite a named peer or agent**: invite tickets are bound to a specific identity key
- **Send signed room messages**: messages are validated from the room event log
- **Share verified artifacts**: files are addressed and checked by BLAKE3 content hash
- **Expose localhost to one allowed room member**: live pipes forward loopback TCP over an authenticated room connection
- **Let agents report status**: invited agents can post signed `agent.status` updates and artifacts

The main product idea is not "another chat app." The wedge is private local collaboration around work in progress: local previews, build artifacts, agent output, and trusted peer access.

## A concrete example

Alice has a web app running on `127.0.0.1:3000` and wants Bob to review it without deploying it or creating a public tunnel URL. Alice creates a room, invites Bob, exposes that local port to Bob only, and keeps the session visible in her local audit log. Bob joins the room and connects to the pipe from his own machine. An invited agent can also post test status or attach a build artifact to the same room.

## What to read first

Start with one of these paths:

- **Try the CLI demo**: follow [`docs/getting-started.md`](docs/getting-started.md)
- **Install or remove a beta binary**: use [`docs/operations/install-uninstall.md`](docs/operations/install-uninstall.md)
- **Join the first builder cohort**: read [`COMMUNITY.md`](COMMUNITY.md) and [`docs/community/first-cohort.md`](docs/community/first-cohort.md)
- **Share a local preview**: follow [`docs/live-pipe-preview.md`](docs/live-pipe-preview.md)
- **Implement or audit the protocol**: read [`docs/protocol.md`](docs/protocol.md)
- **Understand release status**: read [`docs/releases/v0.1.0-rc.3-release-notes.md`](docs/releases/v0.1.0-rc.3-release-notes.md)
- **Review security posture**: read [`docs/security/threat-model.md`](docs/security/threat-model.md)

## Install from source

You need Rust 1.80 or newer and `git`.

```bash
git clone https://github.com/kortiene/iroh-room.git
cd iroh-room
cargo build -p iroh-rooms-cli --release
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/iroh-rooms "$HOME/.local/bin/iroh-rooms"
iroh-rooms --version
```

If `$HOME/.local/bin` is not on `PATH`, choose another user-writable install directory or add it to `PATH`.

For repeatable beta testing, keep room state in an explicit data directory:

```bash
export IROH_ROOMS_HOME="$HOME/.local/share/iroh-rooms-beta"
mkdir -p "$IROH_ROOMS_HOME"
```

Treat that directory as sensitive. It can contain identity secrets, room logs, blob data, invite material in shell history, and `audit.ndjson`.

## Try a room

The full demo uses three local identities: Alice, Bob, and an agent. Each identity needs its own data directory so the CLI behaves like three separate participants.

The guided walkthrough covers the full flow:

```text
identity -> room -> invite -> join -> message -> file -> pipe -> agent status
```

Run it from [`docs/getting-started.md`](docs/getting-started.md). The commands in that guide are reconciled against the shipped binary and backed by the `full_demo_e2e.rs` test suite.

For smaller recipes, use [`docs/community/demo-recipes.md`](docs/community/demo-recipes.md). It has task-focused flows for:

- creating a room
- inviting a peer
- sending a message
- sharing and fetching a file
- exposing a local preview
- inviting an agent

## Current beta scope

`v0.1.0-rc.3` supports:

- local identity and device creation
- room creation
- key-bound room invites and joins
- signed room messages
- verified file sharing
- authenticated loopback-only live TCP pipes
- invited agent participants and signed agent status updates
- local best-effort audit at `<IROH_ROOMS_HOME>/audit.ndjson`
- a Rust SDK facade in `crates/iroh-rooms`, source/workspace use only

The supported binary artifact for this candidate is `x86_64-apple-darwin`. Builders on other platforms should build from source unless a matching release artifact exists.

## Limits you should understand

Read these before trusting Iroh Rooms with real work:

- **No central application server**: peers sync directly through the iroh transport
- **Room-size cap is topology-gated**: no-gossip/full-mesh builds keep the original `MAX_ACTIVE_MEMBERS = 5` hard cap and reject the 6th active join with `RejectReason::RoomFull`. The supported CLI and experimental SDK enable the bounded gossip overlay, which raises the cap to 40; `room members --status` shows `active: <n>/<cap> (<k> slots remaining)`, and `room.active_members.near_cap` fires at one slot below the active cap. Do not raise the cap on a no-gossip full mesh: before the reject existed, N=25 was measured to stop delivering messages entirely (idle `frames_sent=0`, `accepted=0`, a 661 MB inbound backlog) while every connectivity signal still read healthy.
- **No guaranteed offline delivery**: a peer may need to be online and serving for another peer to fetch data
- **Files up to 100 MiB, and the cap is not free**: the enforced share cap is `MAX_SHARED_FILE_BYTES` = 100 MiB. A 100 MiB fetch was measured at ~134.6 MB consumer RSS (the collector allocates the next power of two) and ~2.004x disk use (the payload is written to the out path and re-imported into the blob store), and there is no GC or delete path in non-test code
- **Plaintext local storage**: beta storage is scoped to trusted local machines
- **Invite tickets are secrets**: treat tokens beginning with `roomtkt1` like passwords
- **No native ticket-specific revocation**: Production Beta accepts the bounded leaked-ticket model in [`ADR-0002`](docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md)
- **Local audit only**: `audit.ndjson` is useful for incident reconstruction, not compliance-grade audit
- **Unsigned release artifacts**: archives have SHA-256 checksums but no project signature in this candidate
- **SDK publication is deferred**: the Rust SDK facade exists, but the crate remains `publish = false`

The beta storage, invite, and audit decisions live in:

- [`ADR-0001: Local storage posture`](docs/decisions/ADR-0001-local-storage-posture.md)
- [`ADR-0002: Invite revocation and bounded ticket risk`](docs/decisions/ADR-0002-invite-revocation-bounded-ticket-risk.md)
- [`ADR-0003: Persistent audit posture`](docs/decisions/ADR-0003-persistent-audit-posture.md)

## Error codes

The `iroh-rooms` CLI renders coded failures on stderr as:

```text
error[<code>]: <message>
next: <action>
```

The `next: <action>` line is additive. It does not replace the machine-readable `error[<code>]:` line, so scripts can still match `^error\\[` or branch on `$?`. Advisory receive-path flags render as `warning[<code>]: <message>` and do not make a successful command fail. An uncoded fallback renders as `error: <message>` and exits `1`. stdout is kept for successful command output.

| Exit | Category | Meaning | Example codes |
| ---: | --- | --- | --- |
| `0` | Success | command completed, including best-effort sends that reach zero peers | none |
| `1` | Internal | unexpected or uncoded failure | `internal` |
| `2` | Usage | bad input, missing local state, or unreadable local files | `invalid_room_id`, `invalid_argument`, `no_such_file`, `permission_denied`, `file_too_large`, `identity_not_found`, `room_not_found`, `no_discovery_hint` |
| `3` | Auth | membership, role, capability, identity, room-capacity, or peer authorization denial | `not_a_member`, `unbound_device`, `insufficient_role`, `expired_invite`, `bad_capability`, `room_full`, `wrong_identity`, `peer_unauthorized` |
| `4` | Integrity | signature, event id, encoding, schema, content, room binding, or hash verification failure | `bad_signature`, `id_mismatch`, `non_canonical_encoding`, `invalid_content`, `unknown_schema_version`, `unknown_event_type`, `too_many_parents`, `not_genesis_descended`, `room_id_mismatch`, `hash_mismatch` |
| `5` | Ticket | invite ticket decoding failure | `ticket_bad_prefix`, `ticket_bad_base32`, `ticket_truncated`, `ticket_unsupported_version`, `ticket_bad_checksum`, `ticket_malformed` |
| `6` | Connectivity | peer reachability or blob availability failure | `no_admin_reachable`, `peer_offline`, `blob_unavailable` |

Per-code reference:

| Code | Category | Exit | Meaning | Next action |
| --- | --- | ---: | --- | --- |
| `internal` | Internal | `1` | unexpected internal failure | see the message |
| `invalid_room_id` | Usage | `2` | room id argument does not parse | copy the room id from `room create` or `room members` |
| `invalid_argument` | Usage | `2` | an option value is malformed | see the message |
| `no_such_file` | Usage | `2` | local path or synced file reference is missing | check the path, or run `file list` / `room tail` first |
| `permission_denied` | Usage | `2` | local file cannot be read | check the file's read permissions |
| `file_too_large` | Usage | `2` | local file exceeds the MVP share cap | split or compress the file |
| `identity_not_found` | Usage | `2` | no local identity exists | run `iroh-rooms identity create --name <name>` first |
| `room_not_found` | Usage | `2` | no room with this id is known locally | create a room, or join an invite ticket first |
| `no_discovery_hint` | Usage | `2` | invite ticket has no admin discovery hint | pass `--peer <admin-addr>` |
| `not_a_member` | Auth | `3` | caller is not an active room member | ask the admin to invite you and complete `room join` first |
| `unbound_device` | Auth | `3` | sender has no bound device in membership state | complete `room join` first |
| `insufficient_role` | Auth | `3` | role does not permit the operation | ask the admin to invite you with the intended role |
| `expired_invite` | Auth | `3` | invite expired or was consumed | ask the admin for a fresh `room invite` |
| `bad_capability` | Auth | `3` | invite capability secret does not match | ask the admin to re-issue the invite |
| `room_full` | Auth | `3` | joining would exceed the supported active-member ceiling | remove a member before inviting another |
| `wrong_identity` | Auth | `3` | local identity does not match the ticket binding | run `identity show`, then ask the admin to re-issue the invite for that identity |
| `peer_unauthorized` | Auth | `3` | reachable peer refused this caller | ask the admin to confirm membership has synced |
| `bad_signature` | Integrity | `4` | event signature failed verification | structural rejection, not user-fixable |
| `id_mismatch` | Integrity | `4` | event id does not match signed bytes | structural rejection, not user-fixable |
| `non_canonical_encoding` | Integrity | `4` | event bytes are not canonical CBOR | structural rejection, not user-fixable |
| `invalid_content` | Integrity | `4` | event content failed strict validation | structural rejection, not user-fixable |
| `unknown_schema_version` | Integrity | `4` | event schema version is unsupported | structural rejection, not user-fixable |
| `unknown_event_type` | Integrity | `4` | event type is not in the registry | structural rejection, not user-fixable |
| `too_many_parents` | Integrity | `4` | event exceeds the parent-count bound | structural rejection, not user-fixable |
| `not_genesis_descended` | Integrity | `4` | non-genesis event has no parent | structural rejection, not user-fixable |
| `room_id_mismatch` | Integrity | `4` | event room does not match the command context | structural rejection, not user-fixable |
| `hash_mismatch` | Integrity | `4` | fetched bytes do not match the declared BLAKE3 hash | do not trust this file |
| `ticket_bad_prefix` | Ticket | `5` | ticket prefix is wrong | check the whole ticket was copied |
| `ticket_bad_base32` | Ticket | `5` | ticket body is not valid base32 | check the whole ticket was copied |
| `ticket_truncated` | Ticket | `5` | ticket is incomplete | check the whole ticket was copied |
| `ticket_unsupported_version` | Ticket | `5` | ticket version is not supported | ask the admin for a fresh `room invite` |
| `ticket_bad_checksum` | Ticket | `5` | ticket checksum failed | check the whole ticket was copied |
| `ticket_malformed` | Ticket | `5` | ticket payload is malformed | ask the admin for a fresh `room invite` |
| `no_admin_reachable` | Connectivity | `6` | join could not reach an admin before timeout | ask the admin to run `room tail <ROOM_ID> --accept-joins`, or pass `--peer <admin-addr>` |
| `peer_offline` | Connectivity | `6` | authorized peer is not reachable right now | ask the peer to come online |
| `blob_unavailable` | Connectivity | `6` | no reachable provider currently serves the requested blob | ask a peer that holds it to run `room tail <ROOM_ID>`, then retry `file fetch` |

Verbose network diagnostics are opt-in. `room members <ROOM_ID> --status --verbose` and `room tail <ROOM_ID> --verbose` append stderr-only `diag:` lines with this node's dialable addresses, `relay=...`, per-peer path classifications such as `path=direct`, `path=relay`, `path=mixed`, or `path=none`, and `outbound_depth=<N>` queued-body-byte counters (per-peer encoded `SyncMessage` bytes pending on the outbound byte-priority queue). These diagnostics help explain reachability; they grant no trust and do not expose private key material.

## How it works

Iroh Rooms has three layers:

- **Room event plane**: canonical signed events, membership, deterministic validation, local SQLite persistence, and bounded sync
- **Blob plane**: file import, hash verification, access-controlled serve and fetch
- **Pipe plane**: authenticated TCP-over-QUIC forwarding for loopback services, with explicit per-member allow lists

Room state is derived from an append-only event log. Peers validate signed events locally and converge by syncing the event set they are allowed to see. Access to files and pipes comes from the current membership snapshot, so removed members lose those capabilities.

The implementation uses iroh for peer connectivity and QUIC transport. The protocol contract is documented in [`docs/protocol.md`](docs/protocol.md), and the conformance suite lives under `crates/iroh-rooms-core/tests/`.

## Repository map

```text
crates/iroh-rooms-core/   Protocol, event model, membership, store, sync
crates/iroh-rooms-net/    iroh transport, blob serving, live pipe runtime
crates/iroh-rooms-cli/    CLI binary: identity, room, file, pipe, agent
crates/iroh-rooms/        Rust SDK facade and examples
crates/iroh-rooms-v2-core/  Pure v2 crypto core (unused this phase): canonical CBOR, BLAKE3/Ed25519 ids, governance state machine, fork detection, member Merkle map
crates/spike-nat/         Real-network NAT measurement harness
crates/spike-transport/   Full-mesh versus gossip transport comparison
crates/spike-N40/         Loopback harness measuring where v1 transport breaks at N=40
specs/                    Implementation specs and acceptance criteria
docs/                     Guides, operations docs, ADRs, release records
scripts/                  Verification and release scripts
.adw/                     Optional Switchyard project pack
```

## Verify the workspace

Run the standard local gate:

```bash
scripts/verify.sh
```

Run the developer preview readiness gate before preview release work:

```bash
scripts/release-readiness.sh
```

That script follows [`RELEASE-READINESS.md`](RELEASE-READINESS.md), including the ignored loopback tiers that do not run during the standard workspace gate.

Run the controlled beta readiness gate before release work:

```bash
scripts/production-readiness.sh
```

The production script checks automatable evidence only. Manual sign-off still follows the Production Readiness plan in [`PRODUCTION-READINESS.md`](PRODUCTION-READINESS.md).

The full online demo tiers are ignored by default because they start live loopback sessions. Run them explicitly when validating the end-to-end experience:

```bash
cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1
cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1
```

## Use the SDK

The public Rust facade lives in `crates/iroh-rooms`. It re-exports the stable offline protocol surface by default and gates online runtime APIs behind the `experimental` feature.

Useful starting points:

- [`crates/iroh-rooms/examples/`](crates/iroh-rooms/examples/)
- [`crates/iroh-rooms/examples/example_agent/README.md`](crates/iroh-rooms/examples/example_agent/README.md)
- [`docs/sdk-coverage.md`](docs/sdk-coverage.md)

The SDK crate is not published for `v0.1.0-rc.3`. Use it from the workspace source until the release notes say otherwise.

## Contribute

Good first contributions are concrete and testable:

- run the demo and file a specific setup issue
- try Live Pipe against a real local preview
- test source builds on Linux and macOS variants
- improve diagnostics for confusing CLI errors
- add real-network notes for NAT and relay behavior
- adapt the example agent to a real workflow

Open issues on GitHub with enough detail to reproduce the behavior. Do not attach full invite tickets, `identity.secret`, `rooms.db`, blob contents, unredacted `audit.ndjson`, or full data-directory backups to public reports.

## Community

The first community loop is intentionally small: technical builders who can tolerate beta friction and give precise feedback. The goal is to learn whether private local-first rooms are useful for sharing local previews, artifacts, and agent status without deploying anything.

Read [`COMMUNITY.md`](COMMUNITY.md), then use [`docs/community/demo-recipes.md`](docs/community/demo-recipes.md) to run one workflow. File what worked, what failed, and what felt unclear.

## Project history

The original Phase 0 and Phase 1 planning documents remain in the repository for traceability:

- [`PRD.v0.3.md`](PRD.v0.3.md): current product requirements and MVP scope
- [`PHASE-0-SPIKE.md`](PHASE-0-SPIKE.md): protocol design, architecture decisions, spike plan, and residual risks
- [`PHASE-0-GO-NO-GO.md`](PHASE-0-GO-NO-GO.md): Phase 0 decision record
- [`RELEASE-READINESS.md`](RELEASE-READINESS.md): developer preview release gate
- [`PRODUCTION-READINESS.md`](PRODUCTION-READINESS.md): controlled Production Beta gate

For release-specific facts, prefer the versioned records in [`docs/releases/`](docs/releases/).

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license
