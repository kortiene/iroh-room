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

The Rust workspace is scaffolded but the protocol implementation is not started.
The first implementation target is the Room Event Plane:

1. deterministic signed event model,
2. protocol conformance vectors,
3. SQLite event store,
4. full-mesh iroh QUIC event transport,
5. bounded recent sync and membership fold.

## Repository Layout

```text
crates/iroh-rooms-core/   Core protocol and domain library
crates/iroh-rooms-cli/    CLI binary scaffold
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
