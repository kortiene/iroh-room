# PRD v0.2 — Iroh Rooms: Peer-to-Peer Collaboration Substrate

## 1. Product Summary

**Iroh Rooms** is a local-first, peer-to-peer collaboration substrate built on top of iroh.

It combines:

1. Matrix-inspired rooms and signed events.
2. Sendme-inspired content-addressed file and artifact sharing.
3. Dumb Pipe-inspired live peer-to-peer tunnels.
4. WebRTC-based calls coordinated through iroh.
5. First-class human and AI agent participation.

The goal is not to clone Matrix, Slack, Discord, or Jitsi. The goal is to create a practical foundation for private collaboration spaces where people, devices, and agents can communicate, exchange files, expose live services, and coordinate work without depending on a central application server.

## 2. Product Vision

Create a communication layer where a room is not owned by a SaaS platform or central server.

A room should be a portable, local-first collaboration object that can contain:

1. Messages.
2. Files.
3. Agent updates.
4. Tasks.
5. Decisions.
6. Live logs.
7. Dev previews.
8. Terminal sessions.
9. Call state.
10. Shared artifacts.

The room should work directly between peers when possible, use encrypted relay fallback when necessary, and allow optional user-owned always-on nodes for better availability.

## 3. North Star

A user should be able to create a private room, invite a person or agent with a ticket or QR code, exchange signed messages, share verified files, open a live peer-to-peer tunnel, and keep ownership of the room data locally.

## 4. Positioning

Iroh Rooms is not just a P2P chat app.

It is closer to:

> **A private peer-to-peer workspace runtime for humans and agents.**

The differentiated product idea is that every room can support three collaboration modes:

1. **Conversation** — chat, decisions, tasks, agent updates.
2. **Artifacts** — files, folders, generated outputs, code patches, reports.
3. **Live work** — terminals, logs, web previews, temporary services, future calls.

## 5. Target Users

### Primary users

1. Technical founders building agentic products.
2. Small AI/software teams that need private collaboration spaces.
3. Developers who want local-first rooms for humans and agents.
4. Builders working with MX-Agent, MX-Loom, or similar agent runtimes.
5. Privacy-conscious teams that do not want central message/file storage.

### Secondary users

1. Open-source maintainers coordinating distributed contributors.
2. Local-first app developers.
3. Small organizations that want self-owned collaboration infrastructure.
4. Communities that want private rooms without platform lock-in.

## 6. Problem Statement

Current collaboration platforms usually centralize identity, storage, room membership, files, message history, calls, and integrations.

This creates several problems:

1. The platform owns the collaboration space.
2. Users depend on central accounts and hosted infrastructure.
3. Files and messages are uploaded to third-party servers.
4. Agents are usually integrated through SaaS APIs instead of joining user-owned rooms.
5. Temporary work artifacts like logs, terminals, previews, and dev servers are awkward to share securely.
6. Peer-to-peer tools often solve one narrow use case but do not provide a coherent room model.

## 7. Product Goals

## 7.1 MVP Goals

The MVP should prove that a small private room can support human and agent collaboration without a central application server.

MVP goals:

1. Create a local cryptographic identity.
2. Create a private room.
3. Invite another peer using a ticket.
4. Send signed text messages.
5. Store room history locally.
6. Share files using content-addressed blobs.
7. Fetch shared files from available peers.
8. Open a live peer-to-peer pipe inside a room.
9. Allow a basic agent participant to join and post updates.
10. Provide a CLI-first developer experience.

## 7.2 Long-Term Goals

1. Multi-device identity.
2. QR-code invites.
3. Desktop app.
4. Mobile app.
5. Encrypted local storage.
6. Better room history sync.
7. Optional user-owned always-on node.
8. Agent task rooms.
9. WebRTC 1:1 calls.
10. Small group calls.
11. Optional self-hosted relay/SFU role.
12. Matrix bridge.
13. Jitsi/WebRTC bridge.

## 8. Non-Goals

The MVP should explicitly avoid:

1. Full Matrix federation.
2. Full Matrix client-server API compatibility.
3. Full Jitsi replacement.
4. Building a custom SFU.
5. Large public rooms.
6. Public room discovery.
7. Global usernames.
8. Push notifications.
9. Guaranteed offline delivery.
10. Perfect message ordering across all network conditions.
11. Full group encryption ratchet.
12. Enterprise compliance.
13. Social feed.
14. Public app-store-ready mobile UX.

## 9. Product Architecture

Iroh Rooms should be designed around four planes.

```text
Iroh Rooms
  ├── Room Event Plane
  ├── Blob Plane
  ├── Live Pipe Plane
  └── Call Plane
```

## 9.1 Room Event Plane

The Room Event Plane is the Matrix-inspired layer.

It handles:

1. Room creation.
2. Membership.
3. Text messages.
4. Agent status updates.
5. Task events.
6. File reference events.
7. Pipe session events.
8. Call signaling events in later phases.

Each room is an append-only signed event log.

Example event types:

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
task.created
task.updated
call.started
call.ended
```

The MVP does not need the full complexity of Matrix state resolution. It only needs deterministic validation rules for small private rooms.

## 9.2 Blob Plane

The Blob Plane is the Sendme-inspired layer.

It handles:

1. Files.
2. Folders.
3. Images.
4. Voice notes.
5. Agent artifacts.
6. Code patches.
7. Reports.
8. Room exports.
9. Future call recordings.

The rule is simple:

> Room messages should reference blobs. They should not carry large files directly.

File sharing flow:

```text
1. User adds file locally.
2. File becomes a content-addressed blob or collection.
3. Room creates file.shared event.
4. Peers receive the file reference.
5. Peers fetch the content from available providers.
6. Receiver verifies the content.
```

MVP limitation:

If no peer with the file is online, the file may not be fetchable. This is acceptable for MVP as long as the UX is honest.

Later availability options:

1. Pin file on multiple peers.
2. Add user-owned always-on node.
3. Add room storage peer.
4. Add optional self-hosted storage node.
5. Add retention policy per room.

## 9.3 Live Pipe Plane

The Live Pipe Plane is the Dumb Pipe-inspired layer.

It handles temporary live streams between peers.

Use cases:

1. Agent exposes a local web preview.
2. Agent exposes live logs.
3. Developer exposes a local dev server.
4. QA agent connects to a temporary test service.
5. Human opens a remote terminal session.
6. Agent exposes a debug endpoint.
7. A room participant shares a local tool without making it public.

Example CLI:

```bash
iroh-rooms pipe expose <room-id> --tcp localhost:3000
iroh-rooms pipe connect <room-id> <pipe-id> --local 3001
```

Product value:

This is the feature that makes Iroh Rooms meaningfully different from chat apps.

A room is not only where people talk. It is where live work happens.

MVP limitation:

The MVP should support only simple authenticated TCP forwarding between two peers. Terminal sharing and Unix socket forwarding can come later.

## 9.4 Call Plane

The Call Plane should be realistic.

The MVP should not include calls.

When calls are added, the product should use:

1. Iroh for identity.
2. Iroh room events for call state.
3. Iroh tickets or room membership for invite control.
4. WebRTC for actual media.
5. Optional SFU later.

Call roadmap:

```text
Phase 1: No calls.
Phase 2: Call signaling events only.
Phase 3: 1:1 WebRTC calls.
Phase 4: Small group WebRTC mesh.
Phase 5: Optional peer-selected SFU.
Phase 6: Optional Jitsi bridge.
```

The product should not attempt to rebuild the full Jitsi media stack in early versions.

## 10. MVP Scope

## 10.1 MVP Name

**Iroh Rooms CLI MVP**

## 10.2 MVP Features

1. Local identity creation.
2. Room creation.
3. Invite ticket generation.
4. Join room by ticket.
5. Signed text messages.
6. Local SQLite event history.
7. Basic recent history sync.
8. File sharing through blob references.
9. File fetching from peers.
10. Basic live pipe between two peers.
11. Agent participant identity.
12. Agent can post status updates.
13. CLI interface.
14. Rust core library.

## 10.3 MVP Exclusions

1. Mobile app.
2. Desktop UI.
3. Calls.
4. Full E2EE group ratchet.
5. Multi-device identity.
6. Public rooms.
7. Push notifications.
8. Offline message inbox.
9. Room search.
10. Global identity registry.
11. Built-in payment or billing.
12. Enterprise admin console.

## 11. Core User Journeys

## 11.1 Create Identity

As a user, I can create a local identity.

Acceptance criteria:

1. A local keypair is generated.
2. A device ID is created.
3. A profile name can be set.
4. Identity is stored locally.
5. No central account is required.

Example:

```bash
iroh-rooms identity create --name "Sekou"
```

## 11.2 Create Room

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

## 11.3 Invite Peer

As a user, I can invite another peer.

Acceptance criteria:

1. Invite ticket is generated.
2. Ticket can be copied as text.
3. Ticket can later be rendered as QR code.
4. Ticket gives access to a specific room.
5. Joining peer appears in member list.

Example:

```bash
iroh-rooms room invite <room-id>
```

## 11.4 Send Message

As a participant, I can send a message.

Acceptance criteria:

1. Message is signed.
2. Message is broadcast to connected peers.
3. Message is stored locally.
4. Duplicate event IDs are ignored.
5. Invalid signatures are rejected.

Example:

```bash
iroh-rooms room send <room-id> "I pushed the first prototype."
```

## 11.5 Sync Recent History

As a newly joined participant, I can receive recent room history.

Acceptance criteria:

1. Peer requests recent events.
2. Existing peer returns known events.
3. Receiver validates signatures.
4. Receiver stores missing events.
5. Duplicate events are ignored.

MVP constraint:

This only needs to work for recent history and small rooms. Full decentralized history reconciliation can come later.

## 11.6 Share File

As a participant, I can share a file.

Acceptance criteria:

1. File is added to local blob store.
2. App creates a `file.shared` event.
3. Event contains file metadata and blob hash/reference.
4. Other peers can fetch the file.
5. File integrity is verified after fetch.

Example:

```bash
iroh-rooms file share <room-id> ./prd.pdf
iroh-rooms file fetch <room-id> <file-id>
```

## 11.7 Open Live Pipe

As a participant, I can expose a local service to a room peer.

Acceptance criteria:

1. User exposes a local TCP service.
2. App creates a `pipe.opened` event.
3. Another authorized peer connects to the pipe.
4. Traffic is carried over an encrypted peer connection.
5. User can close the pipe.
6. App creates a `pipe.closed` event.

Example:

```bash
iroh-rooms pipe expose <room-id> --tcp localhost:3000
iroh-rooms pipe connect <room-id> <pipe-id> --local 3001
```

## 11.8 Add Agent Participant

As a user, I can invite an agent to a room.

Acceptance criteria:

1. Agent has its own identity.
2. Agent joins through explicit invite.
3. Agent can post `agent.status` events.
4. Agent can share artifacts.
5. Agent can expose a live preview through a pipe.
6. Agent cannot access rooms unless invited.

Example:

```bash
iroh-rooms agent invite <room-id> <agent-id>
iroh-rooms agent status <room-id> "Running tests..."
```

## 12. Agentic Collaboration Use Case

This product should be especially strong for MX-Agent / MX-Loom style work.

Example room:

```text
Room: Build Iroh Rooms MVP

Participants:
  - Product Owner
  - Backend Agent
  - Frontend Agent
  - QA Agent
  - Human Reviewer

Room contains:
  - product discussion
  - task assignments
  - agent status updates
  - code patches
  - test reports
  - logs
  - live preview URL through pipe
  - final artifacts
```

Agent-specific event types:

```text
agent.joined
agent.status
agent.output
agent.error
agent.artifact.shared
agent.pipe.opened
agent.review.requested
```

This makes the room an execution environment, not just a chat timeline.

## 13. Data Model

## 13.1 Event Envelope

```json
{
  "event_id": "event_01H...",
  "room_id": "room_01H...",
  "sender_id": "user_pubkey",
  "device_id": "device_pubkey",
  "event_type": "message.text",
  "created_at": "2026-06-26T12:00:00Z",
  "content": {
    "body": "Hello room"
  },
  "prev_events": ["event_previous"],
  "signature": "signature_bytes"
}
```

## 13.2 File Shared Event

```json
{
  "event_type": "file.shared",
  "content": {
    "name": "prd.pdf",
    "mime_type": "application/pdf",
    "size_bytes": 204800,
    "blob_hash": "blake3_hash",
    "availability": "peer-provided"
  }
}
```

## 13.3 Pipe Opened Event

```json
{
  "event_type": "pipe.opened",
  "content": {
    "pipe_id": "pipe_01H...",
    "owner_id": "agent_or_user_pubkey",
    "kind": "tcp",
    "label": "frontend-preview",
    "target_hint": "localhost:3000",
    "allowed_members": ["user_pubkey"]
  }
}
```

## 14. Local Storage

Use SQLite for MVP.

Tables:

1. identities
2. devices
3. rooms
4. members
5. events
6. blobs
7. file_references
8. pipes
9. agents
10. peers
11. trust_decisions

Storage principles:

1. Local-first.
2. Append-only events.
3. No central message database.
4. No central file database.
5. Events can be exported.
6. Blobs can be pinned or garbage-collected.
7. User can delete local data.

## 15. Security and Privacy Model

## 15.1 MVP Security

MVP must include:

1. Local cryptographic identity.
2. Device identity.
3. Signed events.
4. Signature validation.
5. Invite tickets treated as capabilities.
6. Room membership checks.
7. Local-only storage.
8. Basic blocklist.
9. Explicit agent invite.
10. Explicit pipe authorization.

## 15.2 Security Not Included in MVP

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

## 15.3 Security Roadmap

Later versions should add:

1. Expiring invites.
2. Invite revocation.
3. Room key rotation.
4. Device verification.
5. Encrypted local database.
6. Recovery phrase.
7. Secure backup.
8. Member removal with key rotation.
9. Pipe access policies.
10. Trust levels for agents.

## 16. Availability Model

The product must be honest about availability.

MVP assumptions:

1. Messages deliver when peers are online or reconnect through available peers.
2. Files are fetchable only when at least one peer with the file is online.
3. Live pipes require both peers to be online.
4. Calls require active peers.
5. There is no cloud inbox in MVP.

Later availability options:

1. User-owned always-on node.
2. Room archive peer.
3. Organization-owned node.
4. Optional self-hosted relay.
5. Optional storage pinning service.
6. Local backup/export.

Product language should say:

> “No central application server by default. Optional infrastructure can improve reliability, but it does not own your room.”

## 17. CLI Requirements

MVP CLI commands:

```bash
iroh-rooms identity create --name "Sekou"
iroh-rooms identity show

iroh-rooms room create "Project Room"
iroh-rooms room invite <room-id>
iroh-rooms room join <ticket>
iroh-rooms room send <room-id> "hello"
iroh-rooms room tail <room-id>
iroh-rooms room members <room-id>

iroh-rooms file share <room-id> ./file.pdf
iroh-rooms file list <room-id>
iroh-rooms file fetch <room-id> <file-id>

iroh-rooms pipe expose <room-id> --tcp localhost:3000
iroh-rooms pipe list <room-id>
iroh-rooms pipe connect <room-id> <pipe-id> --local 3001
iroh-rooms pipe close <pipe-id>

iroh-rooms agent invite <room-id> <agent-id>
iroh-rooms agent status <room-id> "Working..."
```

## 18. Technical Success Metrics

MVP is successful if:

1. Two peers can join a room by ticket.
2. Messages deliver in under 2 seconds when peers are online.
3. Message signatures are validated.
4. Local history survives restart.
5. Recent history sync works after reconnect.
6. Files up to 25 MB can be shared and fetched.
7. Interrupted file fetch can resume or restart cleanly.
8. A live TCP pipe can expose a local dev server to another peer.
9. An agent can join and post status.
10. A room with 5 participants remains usable.
11. The system runs without a central application server.

## 19. Product Success Metrics

Early product success means:

1. Developers understand the room model quickly.
2. Agent workflow feels natural.
3. File sharing feels simpler than uploading to a cloud service.
4. Live pipe feature creates a “wow” moment.
5. Users can explain the product as “private rooms for people and agents.”
6. Teams can run a real small project inside one room.

## 20. Key Risks

## 20.1 P2P Reliability Risk

Some networks will not allow direct connections.

Mitigation:

1. Relay fallback.
2. Clear connection state.
3. Network diagnostics.
4. Optional self-hosted relay later.

## 20.2 Availability Risk

Offline peers cannot serve messages, files, or pipes.

Mitigation:

1. Honest UX.
2. Peer pinning.
3. Always-on node later.
4. Room archive peer later.

## 20.3 Security Risk

Invite tickets can be leaked.

Mitigation:

1. Treat tickets as capabilities.
2. Add expiration.
3. Add revocation.
4. Add membership approval.
5. Add key rotation later.

## 20.4 Scope Risk

Trying to rebuild Matrix, Jitsi, Slack, and Tailscale at the same time would fail.

Mitigation:

1. CLI-first MVP.
2. Small rooms only.
3. No calls in MVP.
4. TCP pipe only.
5. Basic files only.
6. No mobile app at first.

## 20.5 UX Risk

P2P concepts may feel too technical.

Mitigation:

1. Simple room language.
2. Ticket and QR invite.
3. Hide networking details.
4. Explain availability clearly.
5. Start with technical users.

## 21. Roadmap

## Phase 0 — Technical Spike

Duration: 1–2 weeks.

Deliverables:

1. Iroh endpoint setup.
2. Two-peer direct message.
3. Room topic proof of concept.
4. Blob transfer proof of concept.
5. Simple pipe proof of concept.
6. SQLite event log.

## Phase 1 — CLI MVP

Duration: 4–6 weeks.

Deliverables:

1. Identity creation.
2. Room creation.
3. Invite and join.
4. Signed messages.
5. Local history.
6. Recent sync.
7. File share/fetch.
8. TCP pipe expose/connect.
9. Agent status events.
10. Integration tests.

## Phase 2 — Developer Preview

Duration: 4–6 weeks.

Deliverables:

1. Rust SDK.
2. Room protocol documentation.
3. Better error handling.
4. Expiring invites.
5. Pipe access policy.
6. Improved file availability status.
7. Example agent.
8. Example dev-preview workflow.

## Phase 3 — Agent Workspace Alpha

Duration: 6–8 weeks.

Deliverables:

1. Task events.
2. Agent artifact events.
3. Agent review request events.
4. Live logs through pipe.
5. Web preview through pipe.
6. MX-Agent / MX-Loom integration.
7. Room export.

## Phase 4 — Desktop Prototype

Duration: 6–10 weeks.

Deliverables:

1. Tauri desktop app.
2. Room list.
3. Chat timeline.
4. File panel.
5. Pipe panel.
6. Agent cards.
7. QR invite.
8. Local database management.

## Phase 5 — Calls Prototype

Duration: 6–10 weeks.

Deliverables:

1. Call signaling events.
2. 1:1 WebRTC calls.
3. Call state in room timeline.
4. Basic call UI.
5. Small group call experiment.

## Phase 6 — Availability Layer

Duration: 8–12 weeks.

Deliverables:

1. User-owned always-on node.
2. Room archive peer.
3. Blob pinning policy.
4. Better offline catch-up.
5. Optional self-hosted relay configuration.

## 22. Recommended Build Order

Build in this exact order:

1. Rust core crate.
2. Local identity.
3. SQLite event store.
4. Iroh endpoint.
5. Direct peer connection.
6. Room abstraction.
7. Invite ticket.
8. Signed message event.
9. Event validation.
10. Recent history sync.
11. Blob import.
12. File shared event.
13. File fetch.
14. Pipe opened event.
15. TCP pipe expose/connect.
16. Agent identity.
17. Agent status event.
18. CLI polish.
19. Integration tests.
20. Desktop prototype.

## 23. Final Product Thesis

The strongest product is not “Matrix on iroh” and not “Jitsi on iroh.”

The strongest product is:

> **A local-first peer-to-peer room runtime where humans and agents can talk, exchange verified artifacts, expose live work sessions, and eventually launch calls — without a central application server owning the collaboration space.**

The MVP should prove one sharp thing:

> A small group of people and agents can collaborate inside a private room, exchange messages and files, open live peer-to-peer work tunnels, and keep ownership of their data locally.

That is realistic, technically differentiated, and much more valuable than another chat app.
