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

With this the Phase-0 Room Event Plane targets (event model, store, membership fold, sync
engine, identity CLI, room creation, room invite, room join, signed messaging, the offline
room-read CLI, the iroh transport, the live pipe, and the peer connection manager) are all
landed as prototypes.
The Gate-A measurement harness (`nat-probe`, IR-0012) is also landed and CI-proven; what
remains is the manual two-host execution and the Gate-A go/no-go verdict that feeds the
Gate E memo (#15). The remaining feature work is file sharing and agent status (both
tracked separately).

## Repository Layout

```text
crates/iroh-rooms-core/   Core protocol and domain library
crates/iroh-rooms-cli/    CLI binary (identity, room, pipe subcommands; scaffold for file, agent)
crates/iroh-rooms-net/    Full-mesh iroh QUIC transport (IR-0005/IR-0010; ALPNs /iroh-rooms/event/1 + /iroh-rooms/pipe/1)
crates/spike-blobs/       Throwaway blob ACL spike (IR-0009; remove once Blob Plane ships)
crates/spike-nat/         Throwaway Gate-A NAT measurement harness (`nat-probe`, IR-0012)
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
