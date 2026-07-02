# Iroh Rooms

Iroh Rooms is a local-first, peer-to-peer collaboration runtime built on top of
iroh. The MVP target is a CLI-first room where two humans and one agent can
exchange signed messages, share a verified artifact, expose a private live TCP
pipe, and keep room data locally without a central application server.

The Phase 0/1 technical spike and MVP foundation are complete (see "Current
Status" below); the repository is now in Phase 2, "Developer Preview." The
product and protocol source-of-truth documents are:

- `PRD.v0.3.md` — current product requirements and MVP scope.
- `PHASE-0-SPIKE.md` — protocol design, ADRs, spike plan, and residual risks.
- `PRD.md` — historical v0.2 context.

## Getting Started

[`docs/getting-started.md`](docs/getting-started.md) is the copy-pasteable demo walkthrough:
identity → room → invite/join → message → file → live pipe → agent status, with a
troubleshooting guide and the availability model. Every step is implemented and reconciled
against the shipped binary, and the whole two-humans-plus-one-agent flow it describes is
proven end-to-end by the automated `full_demo_e2e.rs` suite (issue #34 / IR-0209, below).

## Protocol

[`docs/protocol.md`](docs/protocol.md) is the implementer reference for the wire/signature/
membership contract — the byte-level rules for building or auditing an interoperable peer,
without reading the whole `PHASE-0-SPIKE.md`.

## Live Pipe Preview

[`docs/live-pipe-preview.md`](docs/live-pipe-preview.md) is a task-focused guide to sharing a
local dev-server or agent-generated preview with one authorized room peer — the
expose/connect/close flow, an agent scenario, and a neutral comparison against public tunnels.

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

**Direct unit + property/fuzz tests for the strict CBOR reader** (risk R1) have
landed in `iroh-rooms-core` and `iroh-rooms-net` (issue #45, an IR-0002
follow-up filed by the #6 review), closing the two test-coverage gaps that
review flagged but did not block on — the reader was previously exercised only
indirectly, through paths that collapse every `CborError` into one
`RejectReason::NonCanonicalEncoding`:

- 25 inline unit tests in `cbor.rs`'s `#[cfg(test)] mod tests`: one case per
  `CborError` variant (all 14, asserting the exact variant, including the
  oversized-declared-length preallocation guard), accept-path + round-trip
  coverage for all five `CborValue` kinds, shortest-form width boundaries,
  canonical map-key ordering (length-first tiebreak), and nesting depth at
  `MAX_DEPTH` (accept) / `MAX_DEPTH + 1` (reject).
- `crates/iroh-rooms-core/tests/cbor_property.rs` — a `proptest`-driven
  robustness suite: no-panic/typed-result over arbitrary bytes for both
  `decode_canonical` and `validate_wire_bytes`, the canonical round-trip
  invariant (`decode(encode(x)) == canonical(x)`), encoder-output-is-always-
  accepted-by-the-reader, and an exhaustive single-bit-flip tamper-evidence
  test over a genuinely valid signed `WireEvent`.
- `crates/iroh-rooms-net/tests/malformed_cbor_e2e.rs` — proves the same
  guarantee on the live QUIC receive path rather than as a pure function call:
  an admitted peer's hostile raw bytes never crash or wedge a `Node`'s sync
  pump and never pollute its store, and the node still correctly ingests a
  valid frame sent right after.
- `proptest = "1"` added to `iroh-rooms-core`'s `[dev-dependencies]` only
  (dev-only, no runtime dependency added); no production code changed.

The **protocol conformance test suite** has landed in
`crates/iroh-rooms-core/tests/` (issue #7 / IR-0003). This is the §-indexed,
traceable conformance binary the `PHASE-0-SPIKE.md` Spike Plan Gate B and Gate D
require — one `#[test]` per spike Protocol Test Vector (§1–§20), all fast,
network-free, and deterministic:

- **`cargo test -p iroh-rooms-core --test protocol_conformance --all-features`**
  runs the full suite. No wall-clock reads, no entropy — every key is seed-derived
  and every clock is injected.
- **Shared `conformance/fixtures.rs`**: deterministic Cast (Alice/Bob/Carol/Dave/Mallory,
  seed-derived keys), Room (`room_id_A`/`room_id_B`), and a fully-assembled fixture-log
  DAG (`E_create … E_pipe`, `E_eq_a/b`, `E_mal`). Tier-1 golden values (CSB,
  `event_id`, signatures, `room_id`) are byte-exact reproductions asserted against
  the spike; Tier-2 fixture-log ids are regenerated from the landed content schema
  and pinned as regression tripwires.
- **Taxonomy completeness gate** (`conformance/taxonomy.rs`): every `RejectReason` (14)
  and `Flag` (3) code is exercised by a named vector or must be on an explicit `DEFERRED`
  list — which is empty (the whole taxonomy is covered). Adding a variant to either enum
  without extending the gate causes the test to fail; a new reason cannot land silently.
- **Traceability table** in `conformance/mod.rs`: the §1–§20 → test-fn map and the
  §8 taxonomy code → vector map, embedded as a module doc comment and machine-checkable
  against the coverage registry.

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

**Room join by ticket** has landed in `crates/iroh-rooms-cli`, `crates/iroh-rooms-core`,
and `crates/iroh-rooms-net` (issue #19 / IR-0104), closing the invite handshake and
making the full two-human exchange end-to-end runnable:

- `iroh-rooms room join <TICKET> [--peer <ENDPOINT_ADDR>]… [--display-name <NAME>] [--timeout <DUR>]`
  — redeems a `roomtkt1…` ticket from `room invite`, becoming an `Active` member whose join
  both peers converge on. Concretely: decodes the ticket (fail-closed), pre-checks the local
  identity matches `ticket.invitee_key` (wrong identity is an actionable error before any
  network IO), brings up an ephemeral `Node`, dials the admin, pulls the never-windowed
  membership sub-DAG via the engine's existing `WantMembership` handshake, assembles and signs
  a `member.joined` via the new `build_member_joined` core builder, self-validates it through
  the full stateless §6 pipeline, fold-checks it locally (bad secret → `BadCapability`,
  expired invite → `ExpiredInvite`, role mismatch → `InsufficientRole` — all deterministic,
  the same verdict every peer reaches), publishes it to the admin, waits for the local
  `Active` transition, then prints the `JoinSummary` (join `event_id`, room, name, role,
  active-member count, and a next-step hint). A join that never reaches the admin is a
  **failure**, not a silent local success — unlike `send`, join is inherently online.
- `iroh-rooms room tail <ROOM_ID> --accept-joins` — the admin side of the bootstrap: runs
  the existing `room tail` session with the new `--accept-joins` flag, which engages the
  `JoinBootstrapAdmission` gate (provisional admission). Without this flag on the admin's
  node, the joiner's connection is rejected before bytes (the default fail-closed behaviour);
  with it, a genuinely unknown device (a first-time invitee) is admitted **provisionally** —
  served only the secret-free membership sub-DAG and allowed to push a single
  `member.joined`, then upgraded to full membership when the fold accepts the join
  (`upgrade-on-learn`). Chat/file/pipe planes remain off-limits to provisional peers.
- A pure `build_member_joined(...)` assembler lands in `iroh-rooms-core::event::join`,
  the sibling of `build_room_created` / `build_member_invited`: deterministic, clock-/RNG-free,
  accepting the joiner's identity and device signing keys, `via_invite_id`, `capability_secret`,
  `role`, an already-built `DeviceBinding`, optional `display_name`, `prev_events`, and
  `created_at`. The `capability_secret` legitimately lands on the log inside the join (Spike §7
  — the join is the proof of the capability; key-binding + departure-consumption keep a replay
  under another key inert). The CLI holds it in a `Zeroizing` buffer from ticket-decode until
  it is placed in the content and never prints it.
- `JoinBootstrapAdmission` in `iroh-rooms-net::admission` (Approach A): wraps
  `AllowlistAdmission` and changes exactly one outcome — an **unknown** device (no prior
  binding) is `AdmitProvisional` when `accept_joins` is set, otherwise rejected as before.
  Bound-but-inactive devices (removed/left members), fail-closed identities, and all
  non-membership traffic paths are unchanged. Authorization is unchanged: `gate_join` in the
  landed membership fold remains the convergent authority on every peer regardless of how the
  connection was admitted. The audit log gains stable `join.bootstrap.*` vocabulary:
  `bootstrap_admitted` / `bootstrap_upgraded` / `bootstrap_blocked`.
- The privacy trade-off of Approach A (provisional admission discloses the secret-free
  membership sub-DAG to a dialer who knows `room_id` + admin `EndpointId` during an open-invite
  window) is explicitly documented and scoped: no capability secret is disclosed; a dialer who
  fails `gate_join` is not made a member; the window closes when `--accept-joins` is not set.
  Approach B (a dedicated capability-proving join ALPN that gates sub-DAG service on a proof)
  is the documented hardening follow-up and is tracked separately.
- Tests: 15+ core unit tests for `build_member_joined` (determinism, all-field round-trip,
  stateless validation, device-binding enforcement, wrong-room, empty `prev_events`, unicode
  display names, agent role); 15+ unit tests for `JoinBootstrapAdmission` (full decision matrix,
  sticky-departure preservation, fail-closed priority, multi-device, join-window toggling);
  8+ CLI unit tests for dial-set construction, timeout parsing, and rejection-message format;
  and a two-peer loopback integration suite (`net/tests/join_e2e.rs`) covering valid join,
  wrong identity, expired invite, and bad secret — asserting the joiner appears in `room members`
  on **both** peers after sync (AC5).

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

**Basic offline room-read commands** have landed in `crates/iroh-rooms-cli` (issue #21 /
IR-0106), exposing a deterministic, network-free read surface for the local event log — the
developer-workflow and testing tier for inspecting room state without an active network
session or membership requirement:

- `iroh-rooms room tail <ROOM_ID> --offline [--json] [--limit <N>]` — a synchronous one-shot
  read of `<home>/rooms.db`. Renders **all** validated event types (not just messages) in
  canonical `(lamport, event_id)` order. Default text mode: one stable line per row —
  `event=… type=… lamport=… from=… role=… status=… at=…  <summary>` — where the attribution
  prefix is machine-parseable and the summary is human context. `--json` mode emits a single
  JSON array of objects with stable field names and flattened type-specific fields (`body`,
  `file_name`, `pipe_id`, …). `--offline` conflicts with the online-session flags; `--json`
  requires `--offline`. No `Node`, no network, no secret load, no membership check.
- `iroh-rooms room members <ROOM_ID> --json` — emits the fold-derived roster as a single-line
  JSON object `{ room, admin, members: [{identity_id, role, status, is_admin}] }`, mirroring
  `identity show --json`. `--json` conflicts with the online `--status` path.
- **Removed vs left** distinction: `status=left` (voluntary self-departure, a `member.left`
  by the subject) is now shown separately from `status=removed` (an admin-authored
  `member.removed`) in both `room members` and `room tail --offline`. The security lattice is
  unchanged — both are the same zero-capability `Status::Removed` state; the distinction is
  display-only, derived from the log, and admin-removal dominates a concurrent self-leave.
- A new `src/display.rs` helper module backs the display logic for offline members, offline
  tail, and the online `members --status` path so all three surfaces use the same code.
- Two new pure core builders — `build_member_left` / `build_member_removed` — land in
  `iroh-rooms-core::event` as siblings of the invite/join builders. They are not wired to any
  CLI command here; the future `room leave` / `member remove` authoring issues reuse them.
- 35+ tests: `tests/tail_cli.rs` (offline order, validated-event coverage, JSON contract,
  restart determinism, error cases, flag conflicts, secret hygiene, AC3 removed/left/dominance)
  and `tests/room_cli.rs` (JSON roster, AC2/AC4), plus golden `event_id` regression locks for
  both new core builders.

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

Issue **#23 / IR-0108** reconciles that prototype to the PRD's canonical, user-facing contract
(no change to the authorization model, event schema, gate, or splice logic):

- `iroh-rooms pipe close <PIPE_ID>` now takes a **bare pipe id** — the room is inferred from the
  local log (backed by the additive read-only `EventStore::room_ids()`), with an optional
  `--room <ROOM_ID>` disambiguator that fails closed on an unknown or ambiguous pipe.
- The owner's `pipe expose` installs a **stderr audit sink** (`Node::spawn_with_pipe_audit`), so
  an unauthorized connect is rejected **and locally visible** as
  `pipe.connect.rejected:<cause>`; `-v` also logs each accepted connection. stdout stays clean
  for scripting.
- The §13.2.4 security warning names the exposed **target and each allowed member**, and graceful
  owner exit now covers **SIGINT and SIGTERM** (`pipe.closed{owner_exit}`); a hard kill
  (SIGKILL / power loss) still stops forwarding but leaves the pipe open on the log until an
  owner/admin `pipe close` — a documented reachability bound.

The **Gate-A real-NAT measurement harness** has landed in `crates/spike-nat`
(issue #43 / IR-0012), providing the purpose-built `nat-probe` tool and a complete
runbook + results schema for closing the one load-bearing Phase-0 assumption still
without measured evidence. No shipping code changes; this is a throwaway spike crate
on the same pattern as `spike-blobs`:

- **`nat-probe listen [--relay-only] [--loopback] [--seed <N>]`** — stands up a
  minimal `iroh::Endpoint` on the n0 stack (DNS discovery + default relay), serves a
  trivial echo protocol on `/iroh-rooms/nat-probe/1` (no room data; spec §8), prints
  its `EndpointId` and home relay URL.
- **`nat-probe dial <ENDPOINT_ID> …`** — dials purely by `EndpointId` (discovery
  resolves the path), measures TTFB / RTT / throughput, reads the **settled path type
  directly off iroh** (active-addr set on `Endpoint::remote_info` — the `ConnectionType`
  watcher is absent on iroh 1.0.1; see `crates/spike-nat/NOTES.md §2`), and emits a
  `ProbeResult` as human summary on stdout and structured JSON on `--json`.
- **`--relay-only`** suppresses direct paths (`clear_ip_transports`) for a controlled
  relay measurement. **`--loopback`** is the offline self-check (relay disabled, dial
  by `--addr`); it proves the harness works but is NOT Gate A.
- **`ProbeResult`** (spec §5 field table): `scenario`, `direction`, `nat_a`/`nat_b`,
  `established`, `path_type`, `initial_path_type`, `hole_punched`, `ttfb_direct_ms` /
  `ttfb_relay_ms`, `rtt_ms`, `rtt_p90_ms`, `throughput_mbit_s`, `setup_time_ms`,
  `relay_url`, `iroh_version`, `run_at_utc`. TTFB is bucketed into the direct/relay
  column by the settled path type (forced to relay under `--relay-only`) so natural
  and controlled relay runs are directly comparable.
- **Runbook** in `crates/spike-nat/NOTES.md` §4: two hosts on different real networks,
  both directions, natural + `--relay-only`, ≥2 NAT scenarios incl. ≥1 likely-symmetric
  (CGNAT/mobile). Operator-supplied `--nat-a`/`--nat-b`/`--scenario`/`--direction`/
  `--run-at` carry all the context; no wall-clock is read in any decision path.
- **GO/NO-GO rubric** (`crates/spike-nat/NOTES.md` §5): GO iff every scenario
  establishes both directions within ≤10 s via at least relay, ≥1 non-symmetric
  scenario achieves direct, and relay usability meets ≥1 Mbit/s / RTT ≤300 ms. A
  NO-GO is a hard input to the Gate E memo (#15).
- **Results artifact**: `crates/spike-nat/results/` — one JSON per run + a rolled-up
  `results.md` table that drops verbatim into `crates/iroh-rooms-net/NOTES.md` under
  "Gate A (real-network)".
- CI proves the harness builds, its loopback self-check passes (a bidi echo on
  loopback, well-formed `ProbeResult` emitted, path classification correct), and all
  unit tests pass; **the manual two-host execution is still owed** before Gate A
  closes (see `crates/iroh-rooms-net/NOTES.md` for the pending table and Gate E feed).

The **peer connection manager** has landed in `crates/iroh-rooms-net` and
`crates/iroh-rooms-cli` (issue #22 / IR-0107), wiring the landed transport primitives
into a roster-reactive whole and closing the ADR-1 "per-room peer manager" follow-up
(IR-0005 NOTES D6/OQ-6, N6, and the roster-driven dial reconciliation):

- **`PeerManager`** (`net/src/manager.rs`): the room-scoped owner of the outbound dial
  set. It derives the **desired** connection set from the live membership snapshot
  (active members' devices minus self) and **reconciles** the running `dial_loop` set
  against it on every fold change — starting loops for newly-active members, aborting
  and tearing down loops for since-removed ones. Idempotent: an unchanged snapshot is a
  no-op (no loop churn, no spurious `ConnEvent`s). Replaces the flat, never-pruned
  `dial_tasks` list.
- **`SnapshotAdmission`** (`net/src/admission.rs`): the production re-point of the
  accept gate (the IR-0005 NOTES D6/OQ-6 follow-up, now closed). Reads a shared
  `AdmissionView` cell on every `authorize` call so a device removed mid-session begins
  being rejected within one tick — not just at the next process restart. The pump is
  the sole writer; the accept hot path takes a short, non-blocking critical section.
  `AllowlistAdmission` is retained for fixtures and the join-bootstrap overlay;
  `JoinBootstrapAdmission` is now generic and composes with either gate.
- **`OfflineReason`** (`net/src/state.rs`): an additive diagnostic refinement of
  `PeerConnState::Offline` for PRD §16.3 / §18.1. Five values:
  `NeverDialed / Unreachable / TransportError / LinkDropped / Deauthorized`. Never a
  trust input; the four-value `PeerConnState` enum and its pinned label strings are
  unchanged. `PeerTable` gains `set_offline`, `entries()`, `identity_of`, and
  `devices_of` to serve the CLI connection panel.
- **`Node::spawn_room`** (`net/src/node.rs`): the managed-session entry point. The pump
  constructs the manager and `SnapshotAdmission` cell, reconciles them after every
  fold-mutating step and on each anti-entropy tick (≤250 ms reaction), and preserves
  the join-bootstrap provisional path (IR-0104) unchanged. The `RoomReconciler`
  change-detector ensures reconcile is a no-op when the snapshot has not moved.
  `Node::reconcile_now()` is a test hook that forces an immediate reconcile.
- **CLI connection panel** in `room tail <ROOM_ID>`: the `ConnEvent` subscriber prints
  a stable, greppable per-peer status line on every transition:
  `peer <identity-short> device=<device-short> state=<state> [reason=<reason>]`
  and a roster summary: `peers: N connected, M offline, K unauthorized`. Reason strings
  are pinned identically to the `PeerConnState` labels (`unreachable`, `link_dropped`,
  `transport_error`, `deauthorized`). An `Unauthorized` peer is never rendered
  as "offline" (PRD §16.4 honesty rule).
- **`room members <ROOM_ID> --status`**: brings up an ephemeral managed node, waits
  for connections to settle, then prints each member row with `role`, membership
  `status`, and live `conn` field (`connected` / `offline reason=<reason>` /
  `unauthorized` / `self` / `n/a`). No new CLI noun; the existing `room members`
  mental model extended with an opt-in flag.
- **New audit vocabulary**: `peer.deauthorized` (mid-session roster removal — terminal,
  will not redial) and `peer.offline:<reason>` (diagnostic transition refinement).
- **Device-selection promotion**: `build_dial_set`'s active-only, self-excluded device
  selection delegates to `PeerManager::desired_devices` so there is a single
  implementation shared between the CLI send/pipe paths and the runtime manager.
- 7 unit tests for `desired_devices` (purity, self-exclusion, invited-only exclusion,
  removed exclusion, three-actor rooms); live-flip test for `SnapshotAdmission` (proves
  mid-session removal takes effect); full decision-matrix test and `JoinBootstrapAdmission`
  over the live gate; `PeerTable` reason-refinement and label-stability tests.
  **Gate A (real-NAT) remains separately owed** (inherits the IR-0005 residual;
  see `crates/iroh-rooms-net/NOTES.md`).

The **two-peer Phase 1A integration test suite** has landed in
`crates/iroh-rooms-cli/tests/two_peer_e2e.rs` (issue #24 / IR-0109), the PRD §19 Phase 1A
deliverable 8 — the product-level proof that the entire Phase 1A slice works end-to-end
across two isolated participants driven through the real `iroh-rooms` binary:

- **Tiered by CI reliability:** a deterministic, network-free CI tier
  (`full_slice_runs_without_central_server`, `message_persists_across_restart`, plus nine
  harness unit tests) always runs in `cargo test`; an `#[ignore]`-gated online tier
  (membership convergence, live pipe, unauthorized denial) requires two live loopback
  processes and is run with the documented command below.
- **No relay, no discovery, no central application server:** every online step uses the
  hidden `--loopback` flag (`NetMode::Loopback` = `RelayMode::Disabled` + `presets::Minimal`)
  over pure loopback QUIC. The `ChildSession` harness parses each host's `listening:` address
  and threads it into the peer's `--peer` for deterministic dial — proving AC1 structurally.
- **All five acceptance criteria covered:** no-central-server (CI), membership convergence
  (gated), message restart-persistence (CI), authorized pipe bytes (gated), unauthorized
  denial (gated — proven by zero forwarded bytes and the `pipe.connect.rejected:not_allowed`
  owner-stderr signal from the IR-0108 audit sink).
- **Backed at the Node layer:** `join_e2e.rs`, `message_e2e.rs`, and `pipe_e2e.rs` remain
  the always-green CI backstop for the same ACs at the transport layer; the CLI suite adds
  product-level coverage on top.

Run the gated online tier locally (loopback only; no relay, no external tools):

```bash
cargo test -p iroh-rooms-cli --test two_peer_e2e -- --ignored --test-threads=1
```

The **hardened recent-history sync implementation** has landed in `crates/iroh-rooms-core`
(issue #26 / IR-0201), graduating the Phase-0 bounded recent-sync prototype to the MVP
recent-history-sync implementation by persisting the `SyncEngine`'s genuinely
non-rebuildable in-flight state across process restarts:

- **Store schema v2** (`user_version = 2`): a forward-only, additive migration adding five
  derived-cache tables — `sync_state`, `sync_backfill_tokens`, `sync_parked`,
  `sync_parked_missing`, and `trust_decisions` — scoped per room. The authoritative `events`
  and `event_parents` tables are untouched; a v1 database upgrades in place (the five tables
  are created empty). An older binary opening a v2 database fails closed with a typed
  `StoreError::Migration`.
- **Restore on `open`**: `SyncEngine::open` now reloads the persisted orphan park
  (re-validating each `wire` via `validate_wire_bytes` on load — a corrupt or tampered row
  is dropped and logged as `park_corrupt`, never a panic), the unconfirmed admin-tip
  suspicion, the per-author backfill token buckets, and the trust-decision audit log, then
  calls `recompute_completeness` **with the restored suspicion in hand** — so an
  `AdminViewSuspect` fail-closed posture re-arms **before** any access decision is served
  after a restart. A reboot cannot clear a removal-sensitive fail-closed gate.
- **Checkpoint on mutation**: a persistence hook writes each non-rebuildable state change to
  SQLite transactionally inside the single-owner pump — park insert/evict/wake, suspicion
  raise/clear/attempts, token consume/refill (batched per tick), trust-decision record — so
  a crash loses at most one tick of state. Checkpoint faults surface as `SyncError::Store`
  (logged, non-fatal; `events` stays authoritative).
- **Retry survives restart**: the restored park's missing-parent edges re-issue `WantEvents`
  on the first `on_connect`/`on_tick`, gated by the **restored** token buckets (not a fresh
  full budget) — so buffering **and** retry are durable and the anti-amplification bound
  cannot be bypassed by crash-looping.
- **Observable rejection (AC3)**: every invalid-event drop increments `counters().rejected`
  and appends a stable `reject.<code>` entry to the bounded `logs()` ring; the net
  `AuditSink` gains `event_rejected`, called from the `Node` pump on the receive path, so
  rejections are observable without a tracing subscriber (the CLI installs none).
- **Additive `EventStore` sync-cache API** (`store` feature):
  `load/save_sync_state`, `load/save_backfill_tokens`, `load/upsert/delete_parked`,
  `load/append_trust_decision`, and the `ParkedRow`/`SyncStateRow`/`TrustRow` DTOs. The
  `SyncEngine` and `iroh-rooms-net` public surfaces are unchanged; callers require no
  signature changes.
- **`SimNet::restart(peer)`**: drops the engine and re-opens it over the same store,
  enabling fast deterministic proofs of the restore path under shuffle and partition.
- **IR-0201 integration suite** (`crates/iroh-rooms-core/tests/sync_restart.rs`): the AC5
  restart-durability matrix — park, fail-closed re-arming, trust-audit persistence, and
  rate-limit conservation across restart — plus migration tests (v1→v2 additive, `events`
  byte-stable, old binary rejects v2), cache-drop equivalence (dropping the five v2 tables
  and reconnecting converges to the same steady state), and the shuffled-delivery-after-restart
  scenario.

**File import into the blob store** has landed in `crates/iroh-rooms-cli` and
`crates/iroh-rooms-net` (issue #27 / IR-0202), completing the producer/import half of the
Blob Plane and making `iroh-rooms file share` operational:

- `iroh-rooms file share <ROOM_ID> <PATH> [--name <NAME>] [--mime <MIME>]` — active-member
  gate (confirmed via the membership fold before any write); classifies the path against the
  §7 error taxonomy (missing, directory, over-cap at 100 MiB, unreadable — all before any
  write); canonicalizes the path (relative paths such as `./f.txt` are supported); imports the
  file into a durable content-addressed `<home>/blobs/` store (`iroh-blobs` `FsStore`, Copy
  mode — the store holds an independent snapshot, so the original file may change or vanish
  afterwards); independently recomputes BLAKE3-256 over the file and asserts it matches the
  store import hash; draws a random 16-byte `file_id` from the OS CSPRNG; assembles and signs
  a `file.shared` event (carrying the handle, display name, MIME type, byte length, content
  hash, blob format, and asserted providers); self-validates it through the full §6 stateless
  pipeline; fold-checks it; and persists it to the local log. Fully offline — no network is
  contacted.
- `iroh-rooms file list <ROOM_ID> [--json]` — offline read: lists every `file.shared` event in
  the room log with `file_id`, name, size, content hash, and provider status (`you (local)` if
  this node holds the blob, `reference-only` otherwise). No membership requirement. `--json`
  emits a stable `[{"file_id", "name", "size_bytes", "blob_hash", "provider"}]` array.
- `BlobStore` in `iroh-rooms-net::blob` — a thin, dependency-isolating wrapper over the
  `iroh-blobs` filesystem store; all `iroh-blobs` types are confined behind it so a version
  bump touches one file. The `FsStore` holds an exclusive on-disk lock while open;
  `BlobStore::close` (which calls `store.shutdown()`) must be called before the same process
  reopens the same directory (the same-process `file share` → `file list` analogue, and the
  protection described in `FsStore exclusive lock needs shutdown` project memory).
- A pure `build_file_shared` assembler lands in `iroh-rooms-core::event`, the sibling of
  `build_message_text` / `build_pipe_opened`: deterministic, clock-/RNG-free, golden-tested
  with a pinned `event_id` regression lock.
- 35+ tests: CLI integration tests (`file_cli.rs`: small file, missing file, unreadable file,
  hash verification, JSON contract, membership gate, size cap); `cli/src/file.rs` unit tests
  (MIME guessing, path classification, provider labels, name validation, handle encoding,
  env-var test seam); `BlobStore` tests (import hash matches independent BLAKE3, `has` after
  import, durability across reopen, empty file, error codes); `build_file_shared` tests
  (determinism, all-field round-trip, stateless validation, golden event id, signature verifies
  under device key).
- **The serve/fetch half** — `file fetch`, the `iroh-blobs` serve ALPN with the spike's
  two-gate ACL, and honest "no-provider" unavailability — was **deliberately out of scope**
  here and landed as the IR-0204 follow-up below.

**`file.shared` validation hardening** has landed in `crates/iroh-rooms-core::event`
(issue #28 / IR-0203), closing the one gap #27 left open: `parse_file_shared` now enforces
semantic bounds on peer-asserted file metadata at the stateless trust boundary, not just
structural shape. `name` and `mime_type` must be non-empty, control-character-free, and
within `MAX_FILE_NAME_BYTES`/`MAX_MIME_TYPE_BYTES` (255 bytes each); `mime_type` must also be a
well-formed `type/subtype` pair; `size_bytes` is capped at the existing `MAX_SHARED_FILE_BYTES`
(100 MiB) at the event layer, not just the CLI's local pre-import check; an explicit
`providers` array, when present, must be non-empty and no longer than `MAX_FILE_PROVIDERS`
(16). Every violation returns the existing `RejectReason::InvalidContent` — no new taxonomy
variant, no wire-schema change, and every previously-valid `file.shared` (including the pinned
golden vectors) stays byte-identical and valid. Because `EventStore::insert` is only ever
reached for validated events, an invalid `file.shared` — however it arrives, from a local
build or a remote peer — can never be persisted and therefore never appears in `file list`.

**Agent identity** has landed in `crates/iroh-rooms-cli` (issue #31 / IR-0206), adding the
first-class `agent invite` noun the PRD documents:

- `iroh-rooms agent invite <ROOM_ID> <AGENT_ID> [--expires <DURATION>]` — a thin, delegating
  wrapper over the landed key-bound invite path (`room invite --invitee <AGENT_ID> --role
  agent`, IR-0103): same admin gate, same `capability_hash`, same `member.invited` builder,
  same ticket codec, same IR-0110 error codes. It draws no new authorization decision and mints
  no new event type — it exists so the agent is a discoverable, first-class CLI concept
  (`PRD.v0.3.md` §15.8/§16) rather than a `--role` flag buried under `room invite`.
- An agent is an ordinary principal: it is created with the same `identity create` a human
  uses, and becomes a member solely through this admin-issued, key-bound invite plus its own
  `room join` — there is no implicit room access and no distinct agent principal type. The
  membership fold's `Role::Agent` (least-privileged in the `Agent < Member < Admin` lattice)
  and `gate_active_member`'s `NotAMember` rejection of any non-invited principal were already
  landed (IR-0008); this issue is a CLI surface plus explicit conformance proof, not new
  protocol behavior.
- `iroh-rooms agent status <ROOM_ID> <STATUS> [--message <TEXT>] [--progress <0..100>]
  [--artifact <FILE_ID>...]` (issue #33 / IR-0208) posts a signed `agent.status` event: build →
  self-validate → persist locally (guaranteed) → best-effort push to connected peers, the same
  offline-first/online-best-effort contract as `room send`. Posting is **not** role-gated — any
  active member may post (`gate_active_member`, matching spike §7 "any current member") — the
  CLI noun is a discoverable surface, not a new authorization tier. `status`/`message` are
  bounded (64/4096 UTF-8 bytes) and `related_artifact_ids` (via repeatable `--artifact
  file_<hex>`, the same handle codec `file share`/`file fetch` use) is capped at 16 entries and
  rejected empty-but-present, mirroring `file.shared`'s trust-boundary bounds. The offline `room
  tail [--offline] [--json]` read renders the full row (`state`, `message`, `progress`,
  `artifacts`); the live streaming `room tail` renders only `message.text` today (a known
  display gap, deferred).

**The agent invite flow** has been proven end-to-end by a dedicated conformance suite
(issue #32 / IR-0207), closing the one gap IR-0206 deliberately deferred — AC3, "agent
join is rejected without valid capability" — asserted *through the agent surface*
rather than assumed from the role-agnostic design. No production code changed:
`agent invite`, `room join`, and `gate_join` were already role-agnostic and already
landed (IR-0206/IR-0103/IR-0104).

- `crates/iroh-rooms-cli/tests/agent_invite_flow.rs` packages the issue's four
  Test-Plan legs in one traceable file: admin invite and non-admin rejection are thin
  re-assertions (the exhaustive matrix stays in `agent_cli.rs`), and the new coverage
  is the ticket-rejection leg — a corrupted or truncated agent ticket rejected
  `ticket_*`/exit 5, and an agent ticket redeemed under the wrong identity rejected
  `wrong_identity`/exit 3, both pre-IO with no membership persisted.
- **Code-identity parity tests** mint an `agent`-role ticket and a `member`-role ticket
  from the same admin, corrupt or misredeem them identically, and assert the IR-0110
  code and exit category match byte-for-byte — the durable guard that a future
  refactor cannot special-case the `agent` role without failing a test.
- `crates/iroh-rooms-net/tests/join_e2e.rs` gained `agent`-role mirrors of
  `bad_capability_secret_join_not_accepted` / `expired_invite_join_not_accepted` — the
  online half of AC3 (wrong capability secret, expired invite), deterministic and
  always-green since it drives two in-process `Node`s rather than a live loopback
  session.

### Error codes

The `iroh-rooms` binary (issue #25 / IR-0110) renders every terminal command failure as a
machine-parseable stderr line, `error[<code>]: <message>`, and every accepted-but-flagged
receive-path event as `warning[<code>]: <message>` (never a failure, never a non-zero exit).
`<code>` is a stable string a script can branch on directly; the process exit code is the
coarser category below, aligned with `clap`'s own exit `2` for a usage error. An uncoded
failure (the long tail of prose errors not yet adopted into the taxonomy) still renders
`error: <message>` and exits `1`. stdout is never used for an error or a warning.

| Exit | Category | Meaning | Example codes |
| ---: | --- | --- | --- |
| `0` | — | success (including `room send` reaching zero peers — availability, not failure) | — |
| `1` | Internal | unexpected / uncoded internal error | `internal`, any uncoded failure |
| `2` | Usage | bad input or environment | `invalid_room_id`, `invalid_argument`, `no_such_file`, `permission_denied`, `file_too_large`, `identity_not_found`, `room_not_found`, `no_discovery_hint` |
| `3` | Auth | authorization / capability denial | `not_a_member`, `unbound_device`, `insufficient_role`, `expired_invite`, `bad_capability`, `wrong_identity`, `peer_unauthorized` |
| `4` | Integrity | crypto / structural rejection | `bad_signature`, `id_mismatch`, `non_canonical_encoding`, `invalid_content`, `unknown_schema_version`, `unknown_event_type`, `too_many_parents`, `not_genesis_descended`, `room_id_mismatch`, `hash_mismatch` |
| `5` | Ticket | ticket decode failure | `ticket_bad_prefix`, `ticket_bad_base32`, `ticket_truncated`, `ticket_unsupported_version`, `ticket_bad_checksum`, `ticket_malformed` |
| `6` | Connectivity | reachability / availability | `no_admin_reachable`, `peer_offline`, `blob_unavailable` |

The taxonomy **wraps** the already-pinned protocol/net vocabulary rather than re-listing it:
`bad_signature`/`not_a_member`/… reuse `RejectReason::code()` verbatim (so `room join` and a
`room tail` receive-path drop of the same event report the identical code — the crypto-vs-
authorization split AC), and `ticket_*` reuses the new `TicketError::code()`. A ticket failure
never echoes the raw token or the capability secret — only the redacted reason. An `offline`
peer (authorized, unreachable right now) is never rendered as `unauthorized` (a peer this node
will never talk to), and vice versa; `room members --status` / `room tail` show the connection
panel distinguishing the two live, while `peer_offline` / `peer_unauthorized` are their
command-failure twins (e.g. `pipe connect`). A clock-skewed but otherwise valid event is a
`warning[clock_skew]` advisory only — it is still accepted, ordered, and displayed.

**Actionable next steps (issue #38 / IR-0303).** Every terminal `error[<code>]:` line a
script-facing coded failure can print is immediately followed, on the next stderr line, by a
fixed, secret-free `next: <action>` line naming the concrete next step — a second, additive
render line, never a replacement for the `error[<code>]:` contract above, so a script matching
`^error\[` or branching on `$?` is unaffected. A structural/crypto rejection (Integrity, exit
`4`, minus `hash_mismatch`) and `invalid_argument`/`internal` have no generic next step (the
call-site message already carries the context); every other code below has one.

| Code | Category | Exit | Meaning | Next action |
| --- | --- | ---: | --- | --- |
| `internal` | Internal | `1` | unexpected / uncoded internal error | — (see the message) |
| `invalid_room_id` | Usage | `2` | room id argument does not parse | copy the room id from `room create` / `room members` (form `blake3:<hex>`) |
| `invalid_argument` | Usage | `2` | an option value is malformed | — (see the message) |
| `no_such_file` | Usage | `2` | `file share` path missing, or `file fetch` reference not found/synced | check the path for `file share`, or run `file list` / `room tail` first to sync the reference for `file fetch` |
| `permission_denied` | Usage | `2` | `file share` path exists but cannot be read | check the file's read permissions, or share a copy you can read |
| `file_too_large` | Usage | `2` | `file share` path exceeds the MVP size cap | the MVP share limit is fixed; split or compress the file |
| `identity_not_found` | Usage | `2` | no local identity exists | run `iroh-rooms identity create --name <name>` first |
| `room_not_found` | Usage | `2` | no room with this id is known locally | run `iroh-rooms room create <name>`, or join an invite ticket first |
| `no_discovery_hint` | Usage | `2` | the invite ticket carries no admin discovery hint | pass `--peer <admin-addr>` (the ticket carried no discovery hint) |
| `not_a_member` | Auth | `3` | sender/caller is not a current room member | ask the admin to invite you and complete `room join` first |
| `unbound_device` | Auth | `3` | sender has no device bound in membership state | ask the admin to invite you and complete `room join` first |
| `insufficient_role` | Auth | `3` | sender's role does not permit this event type | ask the admin to invite you with the intended role |
| `expired_invite` | Auth | `3` | the cited invite was consumed or its expiry passed | ask the admin for a fresh `room invite` (optionally with a longer `--expires`) |
| `bad_capability` | Auth | `3` | a join's capability secret did not match the invite | ask the admin to re-issue the invite for your identity id |
| `wrong_identity` | Auth | `3` | local identity ≠ the ticket's bound `invitee_key` | ask the admin to re-issue the invite for your identity id (`identity show`) |
| `peer_unauthorized` | Auth | `3` | a connectivity command was refused as not-a-member | ask the admin to confirm your membership has synced, then retry |
| `bad_signature`, `id_mismatch`, `non_canonical_encoding`, `invalid_content`, `unknown_schema_version`, `unknown_event_type`, `too_many_parents`, `not_genesis_descended`, `room_id_mismatch` | Integrity | `4` | crypto / structural §8 rejections | — (structural; not user-fixable) |
| `hash_mismatch` | Integrity | `4` | a fetched blob's independently recomputed BLAKE3-256 disagrees with the reference | do not trust this file; the reference or a provider may be corrupt — ask for a fresh `file share` |
| `ticket_bad_prefix`, `ticket_bad_base32`, `ticket_truncated`, `ticket_unsupported_version`, `ticket_bad_checksum`, `ticket_malformed` | Ticket | `5` | the ticket token failed to decode (see the message for which check failed) | check the whole ticket was copied (no truncation/whitespace); if it persists, ask the admin for a fresh `room invite` |
| `no_admin_reachable` | Connectivity | `6` | `room join` never observed the admin within the timeout | ask the admin to run `room tail <ROOM_ID> --accept-joins`, then retry; or pass `--peer <admin-addr>` |
| `peer_offline` | Connectivity | `6` | a connectivity command could not reach an authorized peer | ask the owner to come online (run `room tail <ROOM_ID>`), then retry; or pass `--peer <owner-addr>` |
| `blob_unavailable` | Connectivity | `6` | no reachable provider holds the requested blob | ask a peer that holds the file to run `room tail <ROOM_ID>`, then retry `file fetch` |

Every next-action string is a fixed, non-interpolating `&'static str` (`ErrorCode::next_action()`
in `crates/iroh-rooms-cli/src/error.rs`) — structurally incapable of leaking a secret; runtime
context (a path, an id, a resolved `--peer`) stays in the `error[<code>]:` message, never in the
`next:` line.

**Verbose network diagnostics (issue #38 / IR-0303).** `room members <ROOM_ID> --status
--verbose` (`-v`) and `room tail <ROOM_ID> --verbose` append a stderr-only, opt-in `diag:` block
— hidden by default (§18.5 "hide networking details unless needed") — surfacing the network
facts a developer needs to self-diagnose a P2P failure: this node's dialable address(es) + home
relay url, and, per known peer, its live path classification read from iroh's `remote_info`
*active* transport-address set (never inferred from latency — iroh 1.0.1 has no `ConnectionType`
watcher):

```text
diag: local id=<endpoint_id> direct=<ip:port,…|none> relay=<url|none>
diag: peer <short_id> device=<short> state=connected path=direct relay=none
diag: peer <short_id> device=<short> state=connected path=relay  relay=<url>
diag: transport connected=2 (direct=1 relay=1 mixed=0) offline=0 unauthorized=0
```

`path=` is `direct` (a hole-punched UDP path), `relay` (relayed only), `mixed` (both active — not
yet fully hole-punched), or `none` (no active transport — always true for an `offline` or
`unauthorized` peer, which never renders as reachable). The block is purely diagnostic — like
`OfflineReason`, it is a read-only transport observation and never an authorization input — and
never renders a private key, a ticket secret, or a message payload; only public identifiers
(`EndpointId`/`IdentityKey`), connection-state labels, IP socket addresses, and relay URLs.
Without `--verbose` the output is byte-identical to today (AC2: the machine surface — the
`error[<code>]:`/`warning[<code>]:` lines and the category → exit scheme — is unchanged).

**File fetch and verification** — the serve + fetch half of the Blob Plane — has landed in
`crates/iroh-rooms-net` and `crates/iroh-rooms-cli` (issue #29 / IR-0204), closing the gap
#27/#28 left open and completing the PRD §9.2 file-sharing journey:

- **Serve plane** — the `iroh-blobs` ALPN is now a third `.accept()` on the shared `Router`
  (`net::blob::serve::spawn_blob_gate`), gated by a fold-derived two-gate ACL
  (`BlobAclView`): Gate 1 admits a connect only from a QUIC/TLS-proven `endpoint_id` bound to a
  currently-active member; Gate 2 serves a hash only if it is referenced by a valid
  `file.shared` in the room (from the new `SyncEngine::file_shared_hashes`); push and observe
  are always denied. `iroh-rooms room tail` is the "provider stays online" surface — it opts
  a session into serving the blobs it holds; the pump's existing reconciler refreshes the ACL
  cell on every membership **or** newly-synced-`file.shared` change (tracked independently of
  the membership fold-change detector, since a content event never changes `AdmissionView`).
- **`iroh-rooms file fetch <ROOM_ID> <FILE_ID> [--out <PATH>] [--peer …] [--timeout <DUR>]`** —
  resolves the `file.shared` reference (syncing it first if absent), discovers providers from
  `file.shared.providers` (default: the author's device), dials each in order over the
  ACL-gated blobs ALPN, and requires the assembled bytes' independently recomputed BLAKE3-256
  equal the declared hash before saving — `iroh-blobs` bao verified streaming already rejects
  tampered bytes for the requested hash in transit; the recompute additionally catches a
  `file.shared` that declares a hash different from what it references. A hash mismatch is a
  hard stop (never falls through to another provider); an unauthorized peer is denied at the
  provider's connect gate; an unavailable provider is reported honestly within the bounded
  `--timeout` (default 30s), never a hang. The peer-supplied `name` is sanitized to a safe
  basename (path-traversal guard) before it ever touches the filesystem. On success the CLI
  prints `saved:`, `verified:`, `size:`, and `provider:`; the verified bytes are also
  best-effort re-imported into the local blob store so the fetcher becomes a provider too.
- `net::blob::fetch::fetch_blob` / `FetchOutcome` (`Fetched | DeniedAtConnect | DeniedPerHash |
  HashMismatch | Unavailable`) is the verified-fetch client, lifted from the `spike-blobs`
  spike (#13 / IR-0009) essentially verbatim and re-pointed at the real fold instead of a fixed
  fixture — no new authorization model, event schema, or crate version.

**Honest file-unavailable state** has landed in `crates/iroh-rooms-cli` (issue #30 / IR-0205),
a CLI-only, additive follow-up to #29/IR-0204 — no protocol, event schema, network, serve/fetch,
or authorization behaviour changes, only how `file fetch` *names* an already-landed outcome:

- `file fetch`'s terminal failure is now one of three distinct, coded, script-branchable states
  instead of a bare `bail!`/exit 1: `error[blob_unavailable]:` (exit 6, Connectivity — no
  reachable provider served the bytes), `error[peer_unauthorized]:` (exit 3, Auth — every
  reachable provider refused the connection) or `error[not_a_member]:` (exit 3, Auth — the
  caller itself is not an active member, checked before any node bring-up), and
  `error[hash_mismatch]:` (exit 4, Integrity — the new CLI-native code; a fetched blob's
  independently recomputed BLAKE3-256 disagrees with the reference's declared hash).
- A `FetchTally`/`FetchFailure::classify()` aggregate over the per-provider loop's outcomes
  (`DeniedAtConnect` / `DeniedPerHash` / `Unavailable`) decides `Unauthorized` only when
  *every* attempted provider refused the connection; any availability gap in the mix (an
  unreachable or per-hash-denying provider) keeps the honest headline `Unavailable`, since a
  holder may still come online later.
- `ErrorCode::BlobUnavailable` (defined ahead of time in IR-0110, previously unconstructed and
  `#[allow(dead_code)]`) is now live — this issue is what emits it. Both unavailable messages
  carry the PRD §14 availability language verbatim (no central inbox, no guaranteed offline
  delivery) and name the concrete next step (`iroh-rooms room tail <ROOM_ID>`, then retry).
- CLI tests cover the deterministic/offline tier (`file_cli.rs`: non-member pre-check,
  self-only-provider and loop-exhausted `blob_unavailable`); the two live splits
  (`peer_unauthorized`, and `hash_mismatch`'s CLI-level rendering) stay on the `#[ignore]`-gated
  two-peer e2e tier / existing `blob_e2e.rs` + unit coverage, per `two_peer_e2e.rs`'s notes.

The **full two-humans-plus-one-agent demo integration test** has landed in
`crates/iroh-rooms-cli/tests/full_demo_e2e.rs` (issue #34 / IR-0209), the PRD §19 Phase 1B
deliverable 8 — the product-level proof that PRD §6's full ten-step demo runs as a single flow
across three isolated participants (two humans and an agent), driven through the real
`iroh-rooms` binary, without a central application server:

- **Tiered by CI reliability:** a deterministic, network-free CI tier (the full offline
  backbone; restart-validation over every MVP event type — type/count, per-field content, and
  the post-departure membership fold; the agent-posts-but-has-no-admin-privilege pair) always
  runs in `cargo test`. An `#[ignore]`-gated online tier drives the whole cast live: three-way
  membership convergence, a signed message, a live agent-status push, dual file fetch+verify,
  an authorized/denied live-pipe pair, and a restart check against the wire-delivered log.
- **All five acceptance criteria covered**, each with a product-level CLI assertion in this
  suite *and* a green-in-CI lower-layer backstop (the Node-API e2e suites and the two existing
  CLI online suites, `two_peer_e2e.rs` / `agent_e2e.rs`), so gating the online tier loses no
  guaranteed coverage.
- **Executable transcript:** the suite's centerpiece, `full_demo_two_humans_one_agent`, drives
  every step of `docs/getting-started.md`'s demo through the binary in causal order, asserting
  each printed line inline — the automated counterpart to that guide's manual walkthrough.
- Purely additive: no production code, CLI surface, event schema, or migration changed; every
  command the suite drives already shipped.

Run the gated online tier locally (loopback only; no relay, no external tools; serialized to
avoid port/resource contention across three live processes):

```bash
cargo test -p iroh-rooms-cli --test full_demo_e2e -- --ignored --test-threads=1
```

With this the Phase-0 Room Event Plane targets (event model, store, membership fold, sync
engine, identity CLI, room creation, room invite, room join, signed messaging, the offline
room-read CLI, the iroh transport, the live pipe, the peer connection manager, the Phase 1A
two-peer integration test, the hardened recent-history sync, agent identity, agent status, the
CLI error taxonomy, the full Blob Plane — import, serve, fetch, and honest availability
reporting — and the Phase 1B full-demo integration test) are all landed.

The Gate-A measurement harness (`nat-probe`, IR-0012) is also landed and CI-proven; what
remains is the manual two-host execution and the Gate-A go/no-go verdict that feeds the
Gate E memo (#15).

The **D1 transport decision is now measurement-closed** (issue #10 / IR-0006):
`spike-transport` built minimal full-mesh and `iroh-gossip` backends behind one
trait, measured all five ADR-1 comparison dimensions at N=2..5 on
deterministic loopback, and ratified ADR-1 — full-mesh remains the Room Event
Plane transport; gossip is parked as an optional off-critical-path
liveness/admin-tip carrier. See `crates/spike-transport/NOTES.md` for the
measured table and the decision memo.

Phase 2 ("Developer Preview") opens with the **public Rust SDK façade**,
landed in the new `crates/iroh-rooms` crate (issue #36 / IR-0301) — the first
change since the Phase-0/1 loop above closed the MVP that adds a new
consumer-facing surface rather than a new runtime capability:

- `iroh-rooms` re-exports the already-shipped `iroh-rooms-core`/`iroh-rooms-net`
  surface through five domain modules — `identity`, `room`, `events`, `files`,
  `pipes` — plus an `experimental` cargo-feature-gated namespace for the
  online runtime (`session`, `sync`, `store`, `blob`, `pipe_runtime`).
  Re-exports, not re-wraps: a façade type and its `core`/`net` original are
  the identical type, so mixing the façade with a direct `core`/`net`
  dependency never produces two incompatible copies of the same type.
- **Stable = offline/deterministic protocol, experimental = online runtime**
  is the organizing split: the default-features **stable** tier is exactly
  the conformance-tested, byte-stable layer (event authoring/validation, the
  membership fold, the ticket codec), so its API shape implies no post-MVP
  capability (no multi-device, no call plane, no availability layer) by
  construction; every **experimental** item is feature-gated and doc-marked
  `Experimental (unstable API)`.
- `examples/` (`01_identity` … `07_agent_status`, plus
  `offline_author_and_validate`) mirror `docs/getting-started.md`'s demo
  end-to-end as compilable, runnable programs, and every stable module also
  carries doctests. `scripts/verify.sh` now runs `cargo test -p iroh-rooms
  --doc` and builds every example under default features (both otherwise
  skipped by the existing `--all-targets` run).
- `crates/iroh-rooms-cli` migrated its offline authoring path (`identity`,
  `room` create/members, `invite`, and the `build_*` call sites in
  `message`/`file`) to import through the façade instead of
  `iroh-rooms-core` directly — proof the boundary is real, not just
  documented. `docs/sdk-coverage.md` is the full audit: every symbol the CLI
  imports from `core`/`net` maps to a façade path, with none left over.
- `iroh-rooms-core` and `iroh-rooms-net` gained a doc note marking themselves
  implementation crates and pointing at the façade as the supported entry
  point; neither crate's behavior changed.

The **example agent** has landed in `crates/iroh-rooms/examples/example_agent/`
(issue #39 / IR-0304), turning `07_agent_status.rs`'s "seed for an example
agent" doc comment into a runnable program:

- A minimal, arg-driven Rust program that drives a room entirely **through the
  SDK's `experimental` tier** (`Node::spawn`, `node.publish(...)`, …) — not by
  shelling out to the `iroh-rooms` binary — demonstrating the intended
  third-party integration model.
- Sets up its own local identity (`example_agent identity`, no central-service
  credentials), joins a room by the ticket an admin issues with
  `agent invite` (`example_agent join --ticket … --peer …`), posts one or more
  signed `agent.status` updates, and can optionally share one artifact via
  `--artifact <PATH>` (`file.shared`).
- Its capabilities are explicit and limited to the room membership its invite
  ticket granted: admission is seeded solely from the ticket's discovery hint,
  it joins at the least-privileged `agent` role, and it authors only
  `member.joined`/`agent.status`/(optional) `file.shared` — never anything
  implying admin authority. A co-located `README.md` states this posture
  plainly and gives a line-referenced guide for adapting the example into a
  real agent integration.
- Proven by a new `crates/iroh-rooms/tests/example_agent_e2e.rs`: a
  deterministic CI tier (identity persistence round-trip, offline event
  authoring/validation) plus an `#[ignore]`-gated loopback tier that runs the
  built example binary against an in-process admin node and asserts its
  signed `agent.status` appears in the room tail — the issue's Test Plan.

## Repository Layout

```text
crates/iroh-rooms-core/   Core protocol and domain library
crates/iroh-rooms-cli/    CLI binary (identity, room, file, pipe, agent subcommands)
crates/iroh-rooms-net/    Full-mesh iroh QUIC transport (IR-0005/IR-0010; ALPNs /iroh-rooms/event/1 + /iroh-rooms/pipe/1)
crates/iroh-rooms/        Public Rust SDK façade (IR-0301): curated, stability-tiered re-exports + examples/
crates/spike-blobs/       Throwaway blob ACL spike (IR-0009; remove once Blob Plane ships)
crates/spike-nat/         Throwaway Gate-A NAT measurement harness (`nat-probe`, IR-0012)
crates/spike-transport/   Throwaway gossip-vs-full-mesh transport comparison (`transport-probe`, IR-0006)
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
