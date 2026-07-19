# PRD v0.3 - Iroh Rooms: Peer-to-Peer Collaboration Runtime

## 1. Executive Summary

**Iroh Rooms** is a local-first, peer-to-peer collaboration runtime built on top of iroh.

It enables small private rooms where humans, devices, and AI agents can:

1. Exchange signed room events.
2. Share verified files and generated artifacts.
3. Expose live local services through authenticated peer-to-peer pipes.
4. Keep room data under participant control without a central application server.

The product should not be positioned as "Matrix on iroh", "Slack without servers", or "Jitsi on iroh".

The stronger positioning is:

> **A private peer-to-peer workspace runtime for humans and agents.**

The MVP must prove one sharp workflow:

> Two humans and one agent can join a private room, exchange signed messages, share one verified artifact, expose a local web preview through an authenticated peer-to-peer pipe, and keep room data locally without a central application server.

Calls, mobile apps, desktop UX, large rooms, public discovery, and full decentralized history reconciliation are outside the MVP.

## 2. Product Thesis

Modern collaboration platforms centralize identity, room membership, messages, files, integrations, history, and live workflows. This is convenient, but it makes the collaboration space dependent on a platform owner.

Iroh Rooms starts from a different premise:

> A room should be a portable local-first collaboration object, not a SaaS-owned workspace.

Each room can support three collaboration modes:

1. **Conversation** - messages, decisions, task notes, agent updates.
2. **Artifacts** - files, generated outputs, reports, code patches, logs.
3. **Live work** - temporary services, web previews, live logs, debug endpoints, future terminal sessions.

The differentiated feature is **Live Pipe**. A room is not only where people talk; it is where live work can be shared privately between authorized peers.

## 3. Target Users

### 3.1 Primary Users

1. Technical founders building agentic products.
2. Small AI/software teams that need private collaboration spaces.
3. Developers who want local-first rooms for humans and agents.
4. Builders integrating with MX-Agent, MX-Loom, or similar agent runtimes.
5. Privacy-conscious technical teams that do not want central message or file storage by default.

### 3.2 Secondary Users

1. Open-source maintainers coordinating distributed contributors.
2. Local-first application developers.
3. Small organizations that want self-owned collaboration infrastructure.
4. Communities that want private rooms without platform lock-in.

### 3.3 Initial Beachhead

The first adoption wedge should be:

> **Small technical teams using agents to build software, where agents need to share status, artifacts, logs, and live previews with humans without deploying every intermediate result to cloud infrastructure.**

This wedge is narrow enough to build for, but strategically important because it makes Iroh Rooms more than chat.

## 4. Jobs To Be Done

### JTBD 1 - Private Agent Workroom

When I am running one or more AI agents on local or remote machines, I want to invite them into a private room so they can post status, share outputs, and expose live previews without granting them broad access to my SaaS workspace.

### JTBD 2 - Share a Local Preview Without Deploying

When I have a local dev server or agent-generated preview, I want to expose it to one authorized peer inside a room so they can review it without using a public tunnel, cloud deployment, or shared VPN.

### JTBD 3 - Verified Artifact Exchange

When an agent or teammate produces a file, report, patch, or build artifact, I want to share a verifiable content-addressed reference so recipients can fetch and validate exactly what was shared.

### JTBD 4 - Own the Collaboration Record

When a small team discusses work, makes decisions, and exchanges artifacts, I want the room history to live locally with participants rather than only inside a central vendor database.

## 5. Problem Statement

Current collaboration platforms create several problems for agentic and privacy-sensitive software work:

1. The platform owns the collaboration space.
2. Users depend on central accounts and hosted infrastructure.
3. Messages and files are uploaded to third-party servers by default.
4. Agents are usually integrated through SaaS APIs rather than joining user-owned rooms.
5. Temporary work artifacts like logs, previews, local services, and debug endpoints are awkward to share securely.
6. Peer-to-peer tools often solve one narrow use case but do not provide a coherent room model.
7. Developers frequently use public tunnels or cloud deploys to share temporary work that should remain private.

## 6. MVP Product Goal

The MVP should prove that a small private room can support human and agent collaboration without a central application server.

The MVP is successful if a technical user can complete this demo:

1. Create a local identity.
2. Create a private room.
3. Invite another human peer using a ticket.
4. Invite or configure one agent participant.
5. Send and receive signed text messages.
6. Persist local room history across restart.
7. Share one content-addressed file or artifact.
8. Fetch and verify that artifact from an available peer.
9. Expose a local web preview through a peer-to-peer TCP pipe.
10. Connect to that pipe from an authorized peer.

## 7. MVP Scope

### 7.1 MVP Name

**Iroh Rooms CLI MVP**

### 7.2 In Scope

1. Rust core library.
2. CLI-first developer experience.
3. Local cryptographic identity.
4. Device identity.
5. Room creation.
6. Invite ticket generation.
7. Join room by ticket.
8. Signed text message events.
9. Local SQLite event history.
10. Basic recent history sync for small rooms.
11. Content-addressed blob import.
12. File shared event.
13. File fetch from available peers.
14. Basic authenticated TCP pipe between two peers.
15. Agent identity.
16. Agent can post status updates.
17. Integration tests for the critical two-peer and agent workflow.

### 7.3 Out of Scope

1. Mobile app.
2. Desktop app.
3. Calls.
4. WebRTC media.
5. Full Matrix federation.
6. Full Matrix client-server API compatibility.
7. Full group encryption ratchet.
8. Multi-device identity.
9. Public rooms.
10. Public room discovery.
11. Global usernames.
12. Push notifications.
13. Guaranteed offline delivery.
14. Full decentralized history reconciliation.
15. Room search.
16. Built-in billing.
17. Enterprise admin console.
18. Public app-store-ready UX.

## 8. MVP Cut Line

If schedule or technical risk increases, preserve the product's differentiated core and cut secondary capabilities.

### 8.1 Must Keep

1. Local identity.
2. Room creation.
3. Invite and join.
4. Signed text messages.
5. Local event history.
6. Authenticated two-peer TCP pipe.
7. Basic agent status event.

### 8.2 Cut First

1. File fetch resume.
2. Folder sharing.
3. Multiple blob providers.
4. Advanced file availability status.
5. Rich task events.
6. Agent-specific command surface beyond status posting.
7. QR codes.

### 8.3 Cut Only If Necessary

1. File sharing.
2. Recent history sync.

If both file sharing and recent sync are at risk, the MVP should prioritize **Live Pipe** because that is the strongest differentiation from chat products.

## 9. Product Architecture

Iroh Rooms should be designed around three MVP planes and one future plane.

```text
Iroh Rooms
  |-- Room Event Plane
  |-- Blob Plane
  |-- Live Pipe Plane
  `-- Future Call Plane
```

### 9.1 Room Event Plane

The Room Event Plane is the Matrix-inspired layer.

It handles:

1. Room creation.
2. Membership.
3. Text messages.
4. Agent status updates.
5. File reference events.
6. Pipe session events.
7. Later task, call, and richer agent events.

Each room is an append-only signed event log.

MVP event types:

```text
room.created
member.invited
member.joined
member.left
message.text
file.shared
pipe.opened
pipe.closed
agent.status
```

Post-MVP event types:

```text
task.created
task.updated
agent.output
agent.error
agent.artifact.shared
agent.review.requested
call.started
call.ended
```

The MVP does not need Matrix-style state resolution. It needs deterministic validation rules for small private rooms.

### 9.2 Blob Plane

The Blob Plane is the Sendme-inspired layer.

It handles:

1. Files.
2. Agent artifacts.
3. Reports.
4. Code patches.
5. Room exports in later versions.

Rule:

> Room events should reference blobs. They should not carry large files directly.

MVP file sharing flow:

```text
1. User adds file locally.
2. File becomes a content-addressed blob.
3. Room creates file.shared event.
4. Peers receive the file reference.
5. Peer fetches content from an available provider.
6. Receiver verifies content against the declared hash.
```

MVP limitation:

If no peer with the file is online, the file may not be fetchable. This is acceptable for MVP if the CLI reports the state clearly.

### 9.3 Live Pipe Plane

The Live Pipe Plane is the product's most differentiated layer.

It handles temporary live streams between authorized peers.

MVP use cases:

1. Developer exposes a local dev server to one room peer.
2. Agent exposes a generated web preview.
3. Agent exposes live logs.
4. QA agent connects to a temporary test service.

MVP limitation:

The MVP should support only authenticated TCP forwarding between two peers. Terminal sharing, Unix socket forwarding, multiplexed services, and browser-native UX can come later.

Example:

```bash
iroh-rooms pipe expose <room-id> --tcp localhost:3000 --allow <member-id>
iroh-rooms pipe connect <room-id> <pipe-id> --local 3001
```

### 9.4 Future Call Plane

Calls are not part of the MVP.

When calls are added, the product should use:

1. Iroh for identity and peer discovery.
2. Iroh room events for call state.
3. Room membership for call authorization.
4. WebRTC for media.
5. Optional SFU only after direct 1:1 and small group experiments are validated.

Future call roadmap:

```text
Phase 1: Call signaling events only.
Phase 2: 1:1 WebRTC calls.
Phase 3: Small group WebRTC mesh experiment.
Phase 4: Optional peer-selected SFU.
Phase 5: Optional Jitsi bridge.
```

## 10. Protocol Decisions Required Before Build

These decisions should be made before implementation starts. Leaving them implicit will create rework.

### 10.1 Event Identity

Decision required:

1. Event ID format.
2. Whether event ID is random, hash-derived, or both.
3. Collision behavior.
4. Stable canonical representation.

Recommendation:

Use a hash-derived event ID over canonical serialized event bytes excluding transport metadata.

### 10.2 Canonical Serialization

Decision required:

1. JSON canonicalization vs binary encoding.
2. Exact signature payload.
3. Schema versioning strategy.

Recommendation:

Use a deterministic canonical encoding for signed payloads. Human-readable JSON examples can remain in docs, but implementation should avoid ambiguous serialization for signatures.

### 10.3 Signature Model

Decision required:

1. Which key signs room events.
2. Relationship between user identity key and device key.
3. Whether agent identity is represented as user-like identity, device-like identity, or distinct principal type.

Recommendation:

For MVP, each participant has a local identity key and device key. Events are signed by the device key and validated against room membership records that bind participant identity to authorized device keys.

### 10.4 Membership Validation

Decision required:

1. Who can invite.
2. Who can remove.
3. Whether membership events require admin signature.
4. How joins are validated from tickets.

Recommendation:

For MVP, room creator is admin. Only admin can create invite tickets. A joining member is valid if they present a valid room ticket and emit a signed `member.joined` event.

### 10.5 Invite Capability Format

Decision required:

1. Ticket contents.
2. Ticket expiration.
3. Ticket use count.
4. Revocation support.
5. Whether ticket grants membership automatically or requires approval.

Recommendation:

MVP tickets should be scoped to one room and include room ID, inviter identity, relay/discovery hints, optional expiry, and a secret capability. Expiration should be supported from the start even if revocation is deferred.

### 10.6 Pipe Authorization Lifecycle

Decision required:

1. Whether pipes are open to all room members or explicit members.
2. How long pipe authorization lasts.
3. Whether a pipe can be reconnected.
4. What audit events are written.

Recommendation:

MVP pipes should default to explicit allowed members, have a session ID, be closed explicitly or on process exit, and create `pipe.opened` and `pipe.closed` events. Pipe connection attempts should be logged locally.

### 10.7 Sync Limits

Decision required:

1. Recent history window.
2. Maximum events returned per sync.
3. Behavior on divergent histories.
4. Duplicate and invalid event handling.

Recommendation:

MVP recent sync should be bounded by count and/or time window. Invalid events are rejected and logged. Deep conflict resolution is deferred.

## 11. Data Model v1

### 11.1 Event Envelope

Example shape:

```json
{
  "schema_version": 1,
  "event_id": "event_blake3_...",
  "room_id": "room_...",
  "sender_id": "participant_pubkey",
  "device_id": "device_pubkey",
  "event_type": "message.text",
  "created_at": "2026-06-26T12:00:00Z",
  "prev_events": ["event_previous"],
  "content": {
    "body": "Hello room"
  },
  "signature": {
    "alg": "ed25519",
    "key_id": "device_pubkey",
    "value": "signature_bytes"
  }
}
```

Implementation note:

The JSON above is documentation shape, not necessarily the signed wire representation.

### 11.2 File Shared Event

```json
{
  "event_type": "file.shared",
  "content": {
    "file_id": "file_...",
    "name": "prd.pdf",
    "mime_type": "application/pdf",
    "size_bytes": 204800,
    "blob_hash": "blake3_hash",
    "availability": "peer-provided"
  }
}
```

### 11.3 Pipe Opened Event

```json
{
  "event_type": "pipe.opened",
  "content": {
    "pipe_id": "pipe_...",
    "owner_id": "participant_pubkey",
    "kind": "tcp",
    "label": "frontend-preview",
    "target_hint": "localhost:3000",
    "allowed_members": ["participant_pubkey"],
    "expires_at": "2026-06-26T13:00:00Z"
  }
}
```

### 11.4 Agent Status Event

```json
{
  "event_type": "agent.status",
  "content": {
    "status": "running_tests",
    "message": "Running integration tests",
    "related_artifact_ids": []
  }
}
```

## 12. Local Storage

Use SQLite for MVP.

Required tables:

1. `identities`
2. `devices`
3. `rooms`
4. `members`
5. `events`
6. `blobs`
7. `file_references`
8. `pipes`
9. `agents`
10. `peers`
11. `trust_decisions`
12. `sync_state`

Storage principles:

1. Local-first.
2. Append-only events.
3. No central message database.
4. No central file database.
5. Exportable events.
6. Pinned or garbage-collected blobs.
7. User-controlled local deletion.

## 13. Security and Privacy Model

### 13.1 Security Requirements for MVP

MVP must include:

1. Local cryptographic identity.
2. Device identity.
3. Signed events.
4. Signature validation.
5. Room membership checks.
6. Invite tickets treated as scoped capabilities.
7. Ticket expiration support.
8. Explicit agent invite.
9. Explicit pipe authorization.
10. Local-only storage by default.
11. Basic blocklist.
12. Clear CLI warnings when exposing a local TCP service.

### 13.2 Pipe Security Requirements

Live Pipe is powerful and risky because it exposes local services.

MVP pipe security must include:

1. Explicit `--allow <member-id>` or equivalent authorization.
2. No default exposure to all room members.
3. Local bind defaults to loopback only.
4. Clear display of exposed target, allowed member, pipe ID, and close command.
5. Pipe closes on process exit.
6. `pipe.closed` event emitted when closed cleanly.
7. Local audit log for pipe open, connect, reject, and close events.
8. No terminal sharing in MVP.

### 13.3 Agent Security Requirements

Agents are first-class participants but should not be implicitly trusted.

MVP agent security must include:

1. Agent has its own identity.
2. Agent joins only through explicit invite.
3. Agent events are signed.
4. Agent cannot access rooms unless invited.
5. Agent cannot open pipes unless authorized by room policy.
6. Agent artifacts are content-addressed and verified like user files.

### 13.4 Not Included in MVP

MVP does not include:

1. Full group E2EE ratchet.
2. Perfect forward secrecy.
3. Advanced key rotation.
4. Secure multi-device recovery.
5. Anonymous credentials.
6. Enterprise compliance.
7. Abuse reporting.
8. Strong spam protection.
9. Full metadata privacy.
10. Strong protection after a ticket has leaked.

### 13.5 Security Roadmap

Later versions should add:

1. Invite revocation.
2. Member removal with key rotation.
3. Device verification.
4. Encrypted local database.
5. Recovery phrase.
6. Secure backup.
7. Trust levels for agents.
8. Room-level pipe policies.
9. Storage encryption.
10. Security review before any public beta.

## 14. Availability Model

The product must be honest about availability.

MVP assumptions:

1. Messages deliver when peers are online or reconnect through available peers.
2. Files are fetchable only when at least one peer with the file is online.
3. Live pipes require both peers to be online.
4. There is no cloud inbox.
5. There is no guaranteed offline delivery.

Product language:

> No central application server by default. Optional infrastructure can improve reliability, but it does not own your room.

Later availability options:

1. User-owned always-on node.
2. Room archive peer.
3. Organization-owned node.
4. Optional self-hosted relay.
5. Optional storage pinning service.
6. Local backup/export.

## 15. Core User Journeys

### 15.1 Create Identity

As a user, I can create a local identity.

Acceptance criteria:

1. Local identity keypair is generated.
2. Device keypair is generated.
3. Profile name can be set.
4. Identity is stored locally.
5. No central account is required.

Example:

```bash
iroh-rooms identity create --name "Sekou"
```

### 15.2 Create Room

As a user, I can create a private room.

Acceptance criteria:

1. Room ID is generated.
2. Creator becomes room admin.
3. Local event log is initialized.
4. `room.created` event is signed and stored.

Example:

```bash
iroh-rooms room create "MX-Loom Build Room"
```

### 15.3 Invite Peer

As a room admin, I can invite another peer.

Acceptance criteria:

1. Invite ticket is generated.
2. Ticket is scoped to one room.
3. Ticket can include expiration.
4. Ticket can be copied as text.
5. Joining peer appears in member list after valid join.

Example:

```bash
iroh-rooms room invite <room-id> --expires 24h
```

### 15.4 Send Message

As a participant, I can send a message.

Acceptance criteria:

1. Message event is signed.
2. Message is broadcast to connected peers.
3. Message is stored locally.
4. Duplicate event IDs are ignored.
5. Invalid signatures are rejected.
6. Non-member senders are rejected.

Example:

```bash
iroh-rooms room send <room-id> "I pushed the first prototype."
```

### 15.5 Sync Recent History

As a newly joined participant, I can receive recent room history.

Acceptance criteria:

1. Peer requests recent events.
2. Existing peer returns events within MVP sync bounds.
3. Receiver validates signatures and membership.
4. Receiver stores missing valid events.
5. Duplicate events are ignored.
6. Invalid events are rejected and logged.

MVP constraint:

This only needs to work for recent history and small rooms. Full decentralized reconciliation comes later.

### 15.6 Share File

As a participant, I can share a file.

Acceptance criteria:

1. File is added to local blob store.
2. App creates a `file.shared` event.
3. Event contains file metadata and blob hash/reference.
4. Other peers can fetch the file from available providers.
5. File integrity is verified after fetch.
6. CLI reports unavailable state honestly when no provider is online.

Example:

```bash
iroh-rooms file share <room-id> ./prd.pdf
iroh-rooms file fetch <room-id> <file-id>
```

### 15.7 Open Live Pipe

As a participant, I can expose a local service to an authorized room peer.

Acceptance criteria:

1. User exposes a local TCP service.
2. User explicitly authorizes one or more members.
3. App creates a `pipe.opened` event.
4. Authorized peer connects to the pipe.
5. Unauthorized peer is rejected.
6. Traffic is carried over an encrypted peer connection.
7. User can close the pipe.
8. App creates a `pipe.closed` event.

Example:

```bash
iroh-rooms pipe expose <room-id> --tcp localhost:3000 --allow <member-id>
iroh-rooms pipe connect <room-id> <pipe-id> --local 3001
iroh-rooms pipe close <pipe-id>
```

### 15.8 Add Agent Participant

As a user, I can invite an agent to a room.

Acceptance criteria:

1. Agent has its own identity.
2. Agent joins through explicit invite.
3. Agent can post `agent.status` events.
4. Agent can share artifacts using file sharing primitives.
5. Agent can expose a live preview through a pipe only when authorized.
6. Agent cannot access rooms unless invited.

Example:

```bash
iroh-rooms agent invite <room-id> <agent-id>
iroh-rooms agent status <room-id> "Running tests..."
```

## 16. CLI Requirements

MVP CLI commands:

```bash
iroh-rooms identity create --name "Sekou"
iroh-rooms identity show

iroh-rooms room create "Project Room"
iroh-rooms room invite <room-id> --expires 24h
iroh-rooms room join <ticket>
iroh-rooms room send <room-id> "hello"
iroh-rooms room tail <room-id>
iroh-rooms room members <room-id>

iroh-rooms file share <room-id> ./file.pdf
iroh-rooms file list <room-id>
iroh-rooms file fetch <room-id> <file-id>

iroh-rooms pipe expose <room-id> --tcp localhost:3000 --allow <member-id>
iroh-rooms pipe list <room-id>
iroh-rooms pipe connect <room-id> <pipe-id> --local 3001
iroh-rooms pipe close <pipe-id>

iroh-rooms agent invite <room-id> <agent-id>
iroh-rooms agent status <room-id> "Working..."
```

CLI UX requirements:

1. Commands should print actionable next steps.
2. Pipe exposure should show a clear security warning.
3. Failed connection states should distinguish offline peer, unauthorized peer, unavailable blob, invalid ticket, and invalid signature.
4. Availability limitations should be explicit, not hidden.
5. Output should be script-friendly where possible.

## 17. Success Metrics

### 17.1 Technical Success Metrics

MVP is successful if:

1. Two peers can join a room by ticket.
2. One agent identity can join or be configured as a room participant.
3. Messages deliver in under 2 seconds when peers are online on a normal network.
4. Message signatures are validated.
5. Invalid signatures are rejected.
6. Non-member events are rejected.
7. Local history survives restart.
8. Recent history sync works after reconnect for a small room.
9. Files up to 100 MiB can be shared and fetched. (The enforced cap is
   `MAX_SHARED_FILE_BYTES` = 104_857_600 bytes / 100 MiB, checked at share, at
   `file.shared` validation, and at fetch buffering; an earlier "25 MB" target
   was never enforced and is corrected here. The cap is not free: a 100 MiB
   fetch was measured at ~134.6 MB consumer RSS because the collector allocates
   the next power of two, and disk use is ~2.004x the fetched bytes because the
   payload is written to the out path and re-imported into the blob store, with
   no GC or delete path in non-test code.)
10. Interrupted file fetch can restart cleanly.
11. A live TCP pipe can expose a local dev server to one authorized peer.
12. Unauthorized pipe connection attempts are rejected.
13. A room with 5 participants remains usable. This is the declared ceiling
    (ADR-1, `PHASE-0-SPIKE.md` §17.1.13): the full-mesh QUIC transport is sized
    for ≤5 peers / ≤10 links, and nothing in code enforces or warns above it.
    Measured reality above the ceiling: at N=25 the system does not deliver
    messages at all (idle `frames_sent=0`, `accepted=0`, 661 MB inbound
    backlog; under load 22 published events produced `accepted=0` room-wide),
    so behavior above 5 participants must not be assumed sensible.
14. The system runs without a central application server.

### 17.2 Developer Experience Metrics

Early developer experience is successful if:

1. Time to create first identity is under 1 minute.
2. Time to create and join first two-peer room is under 3 minutes.
3. Time to expose and connect to first live pipe is under 5 minutes after install.
4. A developer can complete the full demo from docs without maintainer help.
5. At least 80% of test users can correctly explain the MVP availability model after onboarding.

### 17.3 Product Success Metrics

Early product validation is successful if:

1. Developers describe the product as private rooms for people and agents.
2. Users identify Live Pipe as meaningfully different from chat.
3. Agent workflow feels natural enough to use in a real small project.
4. Teams can run a real project thread inside one room.
5. Users prefer the room pipe workflow over public tunnels for private previews in at least one real workflow.

## 18. Key Risks and Mitigations

### 18.1 P2P Reliability Risk

Some networks will not allow direct connections.

Mitigation:

1. Relay fallback.
2. Clear connection state.
3. Network diagnostics.
4. Optional self-hosted relay later.

### 18.2 Availability Risk

Offline peers cannot serve messages, files, or pipes.

Mitigation:

1. Honest CLI language.
2. Clear unavailable states.
3. Peer pinning later.
4. Always-on node later.
5. Room archive peer later.

### 18.3 Security Risk

Invite tickets and pipe access can be misused.

Mitigation:

1. Treat tickets as scoped capabilities.
2. Support ticket expiration in MVP.
3. Require explicit pipe authorization.
4. Log local pipe activity.
5. Add invite revocation and key rotation later.

### 18.4 Scope Risk

Trying to rebuild Matrix, Slack, Jitsi, Tailscale, and a full agent runtime at once would fail.

Mitigation:

1. CLI-first MVP.
2. Small rooms only.
3. No calls in MVP.
4. TCP pipe only.
5. Basic files only.
6. Explicit MVP cut line.

### 18.5 UX Risk

P2P concepts may feel too technical.

Mitigation:

1. Simple room language.
2. Ticket invite flow.
3. Clear CLI outputs.
4. Hide networking details unless needed.
5. Start with technical users.

### 18.6 Protocol Ambiguity Risk

Signed event systems fail when serialization, validation, and membership rules are ambiguous.

Mitigation:

1. Decide canonical serialization before build.
2. Document signature payload.
3. Version event schemas.
4. Create protocol test vectors.
5. Reject invalid or unknown critical fields deterministically.

## 19. Roadmap

### Phase 0 - Technical Spike

Duration: 1-2 weeks.

Deliverables:

1. Iroh endpoint setup.
2. Two-peer direct message.
3. Signed event proof of concept.
4. Blob transfer proof of concept.
5. Simple TCP pipe proof of concept.
6. SQLite event log proof of concept.
7. Protocol decision notes.

### Phase 1A - Minimal Differentiated CLI

Duration: 2-3 weeks.

Deliverables:

1. Rust core crate.
2. Local identity.
3. SQLite event store.
4. Room creation.
5. Invite and join.
6. Signed messages.
7. Basic live TCP pipe expose/connect.
8. Integration test for two peers.

This phase proves the core product shape and Live Pipe differentiation.

### Phase 1B - CLI MVP Completion

Duration: 3-4 weeks.

Deliverables:

1. Recent history sync.
2. Blob import.
3. File shared event.
4. File fetch.
5. Agent identity.
6. Agent status events.
7. Security warnings for pipes.
8. Integration test for human + agent workflow.
9. Demo script and getting started docs.

### Phase 2 - Developer Preview

Duration: 4-6 weeks.

Deliverables:

1. Rust SDK.
2. Room protocol documentation.
3. Protocol test vectors.
4. Better error handling.
5. Invite expiration hardening.
6. Pipe access policy.
7. Improved file availability status.
8. Example agent.
9. Example dev-preview workflow.

### Phase 3 - Agent Workspace Alpha

Duration: 6-8 weeks.

Deliverables:

1. Task events.
2. Agent artifact events.
3. Agent review request events.
4. Live logs through pipe.
5. Web preview through pipe.
6. MX-Agent / MX-Loom integration.
7. Room export.

### Phase 4 - Desktop Prototype

Duration: 6-10 weeks.

Deliverables:

1. Tauri desktop app.
2. Room list.
3. Chat timeline.
4. File panel.
5. Pipe panel.
6. Agent cards.
7. QR invite.
8. Local database management.

### Phase 5 - Availability Layer

Duration: 8-12 weeks.

Deliverables:

1. User-owned always-on node.
2. Room archive peer.
3. Blob pinning policy.
4. Better offline catch-up.
5. Optional self-hosted relay configuration.

### Phase 6 - Calls Prototype

Duration: 6-10 weeks.

Deliverables:

1. Call signaling events.
2. 1:1 WebRTC calls.
3. Call state in room timeline.
4. Basic call UI.
5. Small group call experiment.

## 20. Recommended Build Order

Build in this order:

1. Rust core crate.
2. Protocol decision records.
3. Local identity.
4. Device identity.
5. SQLite event store.
6. Iroh endpoint.
7. Direct peer connection.
8. Room abstraction.
9. Invite ticket.
10. Signed message event.
11. Event validation.
12. Membership validation.
13. Basic room tail.
14. Pipe opened event.
15. TCP pipe expose/connect.
16. Pipe authorization.
17. Recent history sync.
18. Blob import.
19. File shared event.
20. File fetch.
21. Agent identity.
22. Agent status event.
23. CLI polish.
24. Integration tests.
25. Demo script and docs.

## 21. Open Questions

These should be resolved before or during Phase 0:

1. Which exact iroh primitives should represent room discovery and peer connection?
2. Should event IDs be pure content hashes or content hash plus local monotonic metadata?
3. What canonical encoding should be used for signed payloads?
4. What is the minimum viable room membership model for admin, member, and agent?
5. Should invite expiration be mandatory or optional in MVP?
6. What is the maximum recent sync window for MVP?
7. How should the CLI present relay fallback and direct connection state?
8. What is the local database path and backup/export story?
9. What are the first protocol test vectors?
10. How will MX-Agent or MX-Loom authenticate as an agent participant?

## 22. Final Recommendation

Iroh Rooms should pursue a narrow but differentiated MVP:

> **A CLI-first local-first room where two humans and one agent can exchange signed messages, share verified artifacts, and expose a private live preview through an authenticated peer-to-peer pipe.**

The product should explicitly avoid becoming a broad chat, call, or federation platform too early.

The strategic bet is that agentic software work needs a collaboration substrate that is private, local-first, artifact-aware, and capable of live work. If the MVP makes Live Pipe and agent participation feel natural, Iroh Rooms becomes meaningfully different from existing collaboration tools.
