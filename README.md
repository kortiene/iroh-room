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

Remaining Room Event Plane targets:

1. Full-mesh iroh QUIC event transport,
2. bounded recent sync and membership fold.

## Repository Layout

```text
crates/iroh-rooms-core/   Core protocol and domain library
crates/iroh-rooms-cli/    CLI binary scaffold
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
