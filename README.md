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

Remaining Room Event Plane targets:

1. Full-mesh iroh QUIC event transport — the real iroh adapter (`crates/iroh-rooms-net`,
   ALPN `/iroh-rooms/event/1`), deferred from IR-0007 (OQ-1) to keep the deterministic
   conformance path iroh-free.

## Repository Layout

```text
crates/iroh-rooms-core/   Core protocol and domain library
crates/iroh-rooms-cli/    CLI binary (identity subcommand; scaffold for room, file, pipe, agent)
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
