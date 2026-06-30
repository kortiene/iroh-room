# Iroh Rooms

Iroh Rooms is a local-first, peer-to-peer collaboration runtime built on top of
iroh. The MVP target is a CLI-first room where two humans and one agent can
exchange signed messages, share a verified artifact, expose a private live TCP
pipe, and keep room data locally without a central application server.

This repository is currently in Phase 0: technical spike and MVP foundation.
The product and protocol source-of-truth documents are:

- `PRD.v0.3.md` — current product requirements and MVP scope.
- `PHASE-0-SPIKE.md` — protocol design, ADRs, spike plan, and residual risks.
- `PRD.md` — historical v0.2 context.

## Getting Started

[`docs/getting-started.md`](docs/getting-started.md) is the copy-pasteable demo walkthrough:
identity → room → invite/join → message → file → live pipe → agent status, with a
troubleshooting guide and the availability model. It is drafted against the planned CLI MVP
(see issue #34) and becomes runnable end-to-end once that CLI lands.

## Current Status

The **canonical signed event model** has landed in `iroh-rooms-core::event`
(issue #6 / IR-0002). This is the byte-for-byte trust boundary the rest of the
Room Event Plane builds on:

- Deterministic-CBOR encoding (RFC 8949 §4.2.1 canonical profile, purpose-built codec).
- BLAKE3-256 event-ID derivation and Ed25519 sign/verify under `device_id`.
- `WireEvent` envelope with verbatim signed-byte preservation for storage and forwarding.
- Strict per-type content validation: unknown-key rejection, length/enum bounds.
- Stateless `validate_wire_bytes` pipeline (Event Protocol §6 stateless subset)
  returning a `ValidatedEvent` or a typed `RejectReason`.
- 70 conformance tests including byte-exact golden vectors (242-byte CSB, `event_id`,
  signature, `room_id_A/B`).

The **SQLite event store** has landed in `iroh-rooms-core::store` behind the
`store` cargo feature (issue #8 / IR-0004). It provides the persistence layer
the membership fold and sync layers will build on:

- `events` table (`STRICT`, WAL, `user_version = 1`): authoritative
  `(event_id, wire)` columns + a denormalized derived cache (`room_id`,
  `sender_id`, `device_id`, `event_type`, `created_at`, `lamport`, `admin_seq`).
- `event_parents` edge table modelling `prev_events` with dangling-parent
  tolerance (out-of-order delivery records the edge; `lamport` stays `NULL` until
  the parent arrives — no error).
- Idempotent insert (`InsertOutcome::Inserted | Duplicate`) with an integrity
  guard re-deriving `BLAKE3(wire.signed)` against the supplied `event_id`.
- Query surface for the sibling fold/sync layers: `contains` / `get` / `count`,
  `parents_of` / `children_of` / `missing_parents`, `room_tail`, `by_type` /
  `by_sender`, `heads`, `admin_chain_tip`.
- `rebuild()`: clears all derived state and recomputes it purely from the
  authoritative `(event_id, wire)` rows — the restart-determinism oracle.
- 24 tests (19 in-module + 5 file-backed e2e) covering all acceptance criteria.

The **deterministic membership fold and authorization layer** has landed in
`iroh-rooms-core::membership` (issue #12 / IR-0008). This is the second stateful
layer of the Room Event Plane, downstream of the stateless validator:

- `RoomMembership`: ingests `ValidatedEvent`s in any order; buffers causally-
  incomplete events (no error — out-of-order tolerance); re-evaluates when
  missing parents arrive.
- **Ancestor-stable authorization**: every event's log-validity is judged only
  against its own causal ancestors, so any two peers holding the identical
  validated set compute a byte-identical verdict regardless of arrival order
  (the §0 same-set convergence guarantee).
- **Removed-dominates causal fold**: per-subject status derived from causal heads
  (`Invited < Active < Removed` lattice max); least-privilege role merge
  (`Agent < Member < Admin` lattice min) tie-broken by lowest `event_id`.
- **Sticky departure**: `member.removed` and `member.left` both consume prior
  invitations; re-admission requires a fresh post-departure `member.invited`.
- **Key-bound invite capabilities only**: a join under a key with no naming
  invite fails the gate, so ban-evasion under a fresh key is blocked.
- `MembershipSnapshot`: the deterministic fold result — per-identity `status`,
  `role`, and bound device; device → identity reverse map for QUIC identity
  resolution (§5).
- **Access-decision predicates** (`blob_serve_allowed`, `pipe_connect_allowed`):
  pure functions the Blob/Pipe planes call; consult the **current snapshot**,
  not the ancestor view — a since-removed member's log-valid events grant zero
  capabilities.
- `validate_with_membership`: completes Event Protocol §6 steps 7–8 on top of
  the stateless `validate_wire_bytes`, via the `MembershipOracle` trait;
  re-exported at `event::validate_with_membership`.
- No `store` feature dependency — the fold is pure in-memory over `ValidatedEvent`s.
- Conformance tests in `tests/membership_fold.rs` covering all six acceptance
  criteria: admin invite/remove, non-admin rejection, key-bound join gate,
  sticky departure, concurrent join/kick convergence to Removed, and
  current-snapshot access decisions.

The **bounded recent-sync engine** has landed in `iroh-rooms-core::sync` behind the
`sync` cargo feature (issue #11 / IR-0007). This is the sync layer over the landed
event/store/membership stack, proving the ADR-2 bounded recent-sync path for MVP-sized
rooms without full decentralized reconciliation:

- `SyncEngine`: a deterministic, sans-IO state machine consuming inbound `SyncMessage`s
  and emitting `Vec<Outgoing>` frames; no async, no clocks beyond an advisory `now_ms`.
- **Never-windowed membership/admin pull** (`WantMembership`): the membership sub-DAG and
  full admin chain are always fully reconciled regardless of chat window size.
- **Bounded recent chat pull** (`WantRecentChat`, `Window { max_count, since_ms }`):
  count-bounded via canonical `(lamport, event_id)` order (trustworthy); `since_ms` is
  advisory only and not a trust input.
- **By-id backfill** (`WantEvents`/`Events`) driven by `Ingest::Buffered.missing` and
  `EventStore::missing_parents`, with §4 anti-amplification bounds (per-author park cap,
  backfill token bucket, depth bound).
- **Admin-tip incompleteness detector + fail-closed** (`AdminTip`, `Completeness`): a node
  whose admin view may be behind a removal **fails closed** on removal-sensitive decisions
  for affected subjects; an admin fork raises a CRITICAL `equivocation` trust decision.
- **`SyncDigest` / `room_event_ids`**: a read-only, additive store helper (no schema
  change) plus the engine's `digest()` — the set-equality oracle the tests assert.
- **`SimNet` harness**: deterministic in-memory multi-peer simulation (seeded shuffle,
  partition, disconnect/reconnect); 37 tests prove arrival-order-independent convergence
  and anti-amplification bounds (Spike Plan Gate D).

The **local identity and device CLI** has landed in `crates/iroh-rooms-cli` (issue #16 /
IR-0101). This is the first real subcommand group in the binary, establishing the
data-directory model and key-persistence layer every later CLI command will reuse:

- `iroh-rooms identity create --name <NAME> [--force]` — generates a participant identity
  keypair (`sender_id`) and a device keypair (`device_id`) from the OS CSPRNG, persists
  them under the resolved data directory with owner-only file permissions (`0600` files,
  `0700` directory on Unix), and refuses to clobber an existing identity without `--force`.
- `iroh-rooms identity show [--json]` — prints `name`, `identity_id`, and `device_id` in a
  script-friendly format (labeled `key: value` lines by default; single-line JSON with
  `--json`). Never reads or prints secret key material.
- Data directory resolution: `--data-dir <PATH>` flag > `IROH_ROOMS_HOME` env >
  platform default (`~/.local/share/iroh-rooms` on Linux,
  `~/Library/Application Support/iroh-rooms` on macOS, `%APPDATA%\iroh-rooms` on Windows).
- Keys are split across two files: `identity.json` (public profile, safe for `show`) and
  `identity.secret` (the only secret-bearing file; `show` never opens it).
- 40+ tests (unit + CLI integration) covering all acceptance criteria, security invariants
  (no secret bytes in any output stream), and Unix file-permission guarantees.

**Room creation** has landed in `crates/iroh-rooms-cli` (issue #17 / IR-0102), wiring the
second subcommand group and completing the genesis-event flow end-to-end:

- `iroh-rooms room create <NAME>` — loads the local identity secrets, draws a 16-byte
  CSPRNG nonce, derives the `room_id` via the §5 `BLAKE3` derivation, assembles and signs
  a `room.created` genesis event (Event Protocol §7), self-validates it through the full
  stateless §6 pipeline, and persists the verbatim wire bytes into `<HOME>/rooms.db`.
  The creator becomes the room's **single immutable admin**. Prints `room_id`,
  `admin` (`identity_id`), and a next-step hint; exits non-zero and writes nothing on any
  error (name validation runs before any IO).
- `iroh-rooms room members <ROOM_ID>` — re-derives the room's membership by re-validating
  and folding the persisted event log; prints `room`, `admin`, and each `member` row with
  `role` and `status`. For a freshly created room this is one row: the creator, `admin`,
  `active`. Room state is **derived from the append-only event log** (no separate
  `rooms`/`members` table), so a room survives CLI restart by design.
- A `build_room_created` pure genesis builder lives in `iroh-rooms-core::event::genesis`,
  deterministic in its inputs (the caller injects the nonce and clock), golden-tested
  against the §5 `room_id` vector, and reusable by future flows.
- Secret hygiene: signing secrets are held in `Zeroizing` buffers and never appear in any
  output or error path.
- 30+ tests (core unit tests including the §5 golden `room_id` vector, CLI integration
  tests via `assert_cmd`) covering all five acceptance criteria.

The **full-mesh QUIC event transport prototype** has landed in
`crates/iroh-rooms-net` (issue #9 / IR-0005). This is the real iroh adapter — the
shipping carrier behind the landed, sans-IO `SyncEngine` — proving the
`PHASE-0-SPIKE.md` ADR-1 path (full-mesh direct QUIC over the custom ALPN):

- `NetTransport`: an `iroh::Endpoint` keyed by the node's `device_id` secret
  (`endpoint.id() == device_id == EndpointId`) + a `Router` carrying ALPN
  `/iroh-rooms/event/1`, implementing `iroh_rooms_core::sync::SyncTransport` so the
  deterministic engine drives it unchanged.
- **Admission before bytes**: the `ProtocolHandler` authorizes the QUIC/TLS-proven
  remote `EndpointId` against a `device → identity → Active?` allowlist (the
  `MembershipSnapshot` shape) and closes the connection **before** `accept_bi()` for
  any non-member — an unauthorized peer's event bytes are never read.
- **Observable connection state** (`PeerConnState`): the PRD §16.3 trichotomy —
  connected / offline / unauthorized — as a snapshot + a live `ConnEvent` stream.
- **Per-peer bidi-stream framing** (length-prefixed canonical-CBOR `SyncMessage`
  frames; live `WireEvent` push = `SyncMessage::Events`) + a dial-with-backoff
  reconnect loop.
- 85 tests: 67 unit tests, a 9-test frame-codec integration suite (`tests/frame.rs`,
  real QUIC loopback), and a 9-test loopback integration suite (`tests/loopback.rs`,
  T1–T9) covering all four acceptance criteria (deterministic, no relay/network).
- **Gate A (real-NAT run) is still owed** before MVP go — the loopback suite is not
  Gate A; see `crates/iroh-rooms-net/NOTES.md`.

**Key-bound room invite** has landed in `crates/iroh-rooms-cli` (issue #18 / IR-0103),
adding the second authoring command and the first out-of-band capability artifact:

- `iroh-rooms room invite <ROOM_ID> --invitee <IDENTITY_ID> [--role member|agent] [--expires <DURATION>]`
  — admin-only; confirms the caller is the room's single immutable admin via the membership
  fold, draws a fresh `invite_id` and capability **secret** from the OS CSPRNG, computes
  `capability_hash = BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)`, assembles
  and signs a `member.invited` event (carrying the hash, **never** the secret), self-validates
  and fold-checks the event before persisting, then emits an out-of-band `RoomInviteTicket`
  token (`roomtkt1…`) carrying the room id, invite id, capability secret, bound invitee key,
  role, optional expiry, and a discovery hint (admin `device_id`).
- `RoomInviteTicket` lives in `iroh-rooms-core::ticket`: a canonical, round-trippable text
  token (`roomtkt1<base32-lowercase-nopad(version ‖ CBOR ‖ BLAKE3-checksum)>`) with a
  redacted `Debug` (secret masked) and a `capability_hash()` method that recomputes the
  on-log hash from the ticket secret (AC4). Placed in `core` so the sibling `room join`
  flow can decode it without duplicating the codec.
- `--expires <DURATION>` supports `<int>{s|m|h|d}` (e.g. `24h`, `7d`); expiry is encoded
  as `expires_at` (ms since Unix epoch) in the event content, log-only and advisory-clock-free
  — enforcement lives in the landed `gate_join`, not the local clock. `expires: never` when
  absent.
- `--role admin` is rejected at the CLI (single immutable admin; the fold has no second-admin
  semantics). `agent` and `member` are accepted.
- Tickets are key-bound to the named `--invitee`; open/bearer tickets are out of scope for MVP.
- Output is script-friendly labeled lines, ending with the ticket token and a password-grade warning.
- 20+ tests (core unit: deterministic builder, golden event id, secret-absent-from-log AC3,
  capability-hash AC4, ticket round-trip + corruption rejection; CLI integration: admin path,
  non-admin rejection, bad args, secret-not-in-output).

**Signed message send and receive** has landed in `crates/iroh-rooms-cli` (issue #20 /
IR-0105), adding the first **online** commands — the first that leave the local filesystem
and drive the `iroh-rooms-net` carrier from the binary:

- `iroh-rooms room send <ROOM_ID> <MESSAGE> [--format plain|markdown] [--reply-to <EVENT_ID>]
  [--peer <ENDPOINT_ADDR>]… [--timeout <DUR>]` — offline-first, online-best-effort: confirms
  the caller is an active member via the membership fold, selects `prev_events = heads`,
  assembles and signs a `message.text` through the new pure `build_message_text` core builder,
  self-validates it, then brings up an ephemeral `Node`, dials the room's other active members,
  and lets the engine `publish` persist and fan the frame out to connected peers. The message
  is **always** stored locally (the guarantee); reaching zero peers is reported, not an error
  (no queue, no guaranteed offline delivery — PRD §14).
- `iroh-rooms room tail <ROOM_ID> [--peer <ENDPOINT_ADDR>]… [--limit <N>]` — the long-running
  receiver/session: brings up a `Node`, prints its dialable `listening:` address (so a LAN/CI
  peer can dial it via `--peer` without discovery), accepts inbound frames (validated, deduped,
  persisted by the landed engine), and renders the timeline in deterministic `(lamport,
  event_id)` order until interrupted (Ctrl-C).
- A pure `build_message_text` (the byte-exact assembly point, golden-tested) lands in
  `iroh-rooms-core::event`, plus thin additive read passthroughs (`SyncEngine::room_tail` →
  `Node::room_tail`) so a running node can surface its timeline for display.
- Every message-correctness criterion — signed by the device key, duplicate event ids ignored,
  invalid signatures rejected, non-member messages rejected, deterministic timeline order — is
  enforced by the landed, conformance-tested validator / membership fold / store / sync engine;
  this issue is the integration and the two new commands, not new correctness logic.
- The full two-human exchange additionally needs `room join` (#19) to make a second participant
  an active member; until that lands the commands run but the round trip is gated on #19. Real-NAT
  delivery inherits the open Gate-A risk from the transport prototype (#9).

The **live TCP pipe prototype** has landed in `crates/iroh-rooms-net` and
`crates/iroh-rooms-cli` (issue #14 / IR-0010). This is the PRD's most differentiated feature
— authenticated TCP-over-QUIC forwarding that exposes a local loopback service to an
**explicitly authorized** room peer, and only to that peer:

- `/iroh-rooms/pipe/1` ALPN chained as the second `.accept()` on the shared `Router`
  (one `Endpoint`, two planes: event + pipe).
- **Two-stage connect gate**: stage 1 closes a non-member / non-Active device before
  `accept_bi()` — no handshake byte is read; stage 2 reads `PipeHello{pipe_id}`, runs
  `pipe_connect_allowed` + `pipe.closed`-known + expiry, and only on `Accept` splices
  QUIC↔loopback TCP. Every lookup fails closed.
- **Tear-down-on-learn** watcher re-evaluates each live session every tick and severs any
  that no longer pass the gate (membership removal, explicit close, or expiry).
- **Loopback-only binds** (PRD §13.2.3): non-loopback `--tcp` targets are refused; the
  connector's local listener binds `127.0.0.1` only.
- Stable, greppable audit vocabulary: `pipe.opened` / `pipe.closed` /
  `pipe.connect.accepted` / `pipe.connect.rejected:<cause>` / `pipe.torndown:<cause>`.
- `iroh-rooms pipe expose | connect | close | list` CLI subcommands with the PRD §13.2
  security warning, loopback enforcement, non-empty `--allow`, active-member pre-check,
  and §16.3 failure-mode distinction.
- `crates/iroh-rooms-net/tests/pipe_e2e.rs` proves P1–P6 (AC1–AC5 + expiry) on
  in-process loopback nodes with an in-test echo server; every await is timeout-bounded.
- **Gate A (real-NAT run for the pipe ALPN) is still owed** before MVP go, inheriting the
  open Gate-A risk from the transport prototype (#9); see `crates/iroh-rooms-net/NOTES.md`.

With this the Phase-0 Room Event Plane targets (event model, store, membership fold, sync
engine, identity CLI, room creation, room invite, signed messaging, the iroh transport, and
the live pipe) are all landed as prototypes; the remaining work is room join, file sharing,
agent status, the `MembershipSnapshot` re-point of admission, and the Gate-A real-network
confirmation (all tracked separately).

## Repository Layout

```text
crates/iroh-rooms-core/   Core protocol and domain library
crates/iroh-rooms-cli/    CLI binary (identity, room, pipe subcommands; scaffold for file, agent)
crates/iroh-rooms-net/    Full-mesh iroh QUIC transport (IR-0005/IR-0010; ALPNs /iroh-rooms/event/1 + /iroh-rooms/pipe/1)
crates/spike-blobs/       Throwaway blob ACL spike (IR-0009; remove once Blob Plane ships)
.adw/                     Switchyard / ADW project pack
scripts/verify.sh         Local and CI verification gate
specs/                    Implementation specs produced during planning
```

## Verify

```bash
scripts/verify.sh
```

The gate runs formatting, Clippy, and tests across the workspace.

## Backlog

The execution backlog lives in GitHub Issues:

- Phase 0 epic: <https://github.com/kortiene/iroh-room/issues/1>
- First engineering slice: <https://github.com/kortiene/iroh-room/issues/5>

## Switchyard / ADW

This repository includes an `.adw` project pack so Switchyard can be used as an
optional contribution orchestrator. Switchyard remains an external tool; it is
not vendored into this repository and is not a runtime dependency of Iroh Rooms.
When Switchyard is run from another checkout, pass `--project-root` pointing at
this repository so it loads `.adw/config.json` and runs `scripts/verify.sh`.

See `CONTRIBUTING.md` for the recommended workflow.
