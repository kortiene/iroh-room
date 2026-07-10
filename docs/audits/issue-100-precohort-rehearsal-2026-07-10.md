# Issue #100 Pre-Cohort Multi-Host Rehearsal

Date: 2026-07-10  
Scope: infrastructure rehearsal for [issue #100](https://github.com/kortiene/iroh-room/issues/100)  
Verdict: **GO** for the first three supervised external builder attempts;
**NO-GO** for broader beta expansion until the network-resource gate below is
closed

## Scope and limits

This run exercised the released `v0.1.0-rc.1` CLI across seven authorized,
independent Linux hosts. It is infrastructure evidence only: it does **not**
satisfy issue #100's requirement for three named external builders or establish
product pull.

All participants used fresh, isolated temporary data directories. Invite
tickets, identity material, room databases, blobs, raw audit logs, endpoint
addresses, and raw command transcripts are deliberately excluded from this
record.

## Build provenance

| Field | Evidence |
| --- | --- |
| Source | annotated tag `v0.1.0-rc.1`, commit `bd26277ecd248ae4c17328e30c48d51bedc6a168` |
| Builder | temporary Rust `1.96.0` toolchain on a Linux x86_64 host; no system packages or global configuration changed |
| Build | `cargo build --locked --release -p iroh-rooms-cli` completed in 78.42 s |
| Binary | `iroh-rooms 0.1.0-rc.1` on every host |
| SHA-256 | `9d69a79a265106e0325b2b80c4c9fc1eab5dd064fe1f9dd1fbd97ec764f8774d` |

Every target host was Ubuntu 22.04/x86_64 with sufficient disk space. None had
Rust, Cargo, or `iroh-rooms` installed beforehand; the tested binary was copied
to a dedicated temporary test directory.

## Results

| Scenario | Hosts | Evidence | Result |
| --- | --- | --- | --- |
| Two humans, message, verified file | `kilo` ↔ `zulu` | Key-bound join completed in 1.655 s; Bob's message reported one connected delivery and appeared on Alice; file fetch completed in 1.155 s and matched the declared BLAKE3 hash and expected bytes. | PASS |
| Private Live Pipe HTTP | `demo1` ↔ `demo2` | Join completed in 1.799 s; a loopback-only HTTP server was reachable through the authenticated pipe in 1.036 s; an independent `demo3` probe could not reach the underlying HTTP port directly; shutdown left `pipe list` empty. | PASS |
| Invited agent status and artifact | `stargate-01` ↔ `stargate-03` | Agent join completed in 1.619 s; `agent status` delivered to one peer in 1.499 s; admin offline JSON contained the agent-role `completed` status, `progress: 100`, and artifact reference; admin fetched the artifact in 1.070 s with hash and content verification. | PASS |

All recorded long-running `room tail`, `pipe expose`, `pipe connect`, and HTTP
server processes were stopped before cleanup. No coded CLI errors were observed
in the successful workflows.

## Findings

### P1 — Cohort prerequisites resolved

1. The file recipe now calls `file fetch ... --peer ...` directly, which
   synchronizes the file event instead of waiting for an event the live tail
   does not render. See
   [demo recipes](../community/demo-recipes.md) and
   [file fetch implementation](../../crates/iroh-rooms-cli/src/file.rs).
2. The Live Pipe recipe now binds the sample server to `127.0.0.1`, preventing
   independent LAN exposure. See
   [demo recipes](../community/demo-recipes.md).
3. The Linux posture is now explicit: the first supervised cohort uses a
   locked, exact-tag source build. A signed Linux x86_64 artifact remains a
   release-process improvement before broader distribution.
4. Blob retrieval is now bounded by the signed file-event size and the protocol
   maximum, and downloaded files use atomic no-clobber persistence. Oversized,
   non-contiguous, truncated, or conflicting destinations fail closed.

### P2 — Observe rather than infer transport path

The tested CLI flows exposed peer reachability but did not produce a durable,
structured direct-versus-relay path record. Do not claim a direct-path result
from this rehearsal; collect Gate A-style diagnostics separately when path
classification matters.

## Decision

Proceed with the first three supervised external builder attempts. Keep the
scope narrow: source-build-capable participants, trusted local machines,
private ticket transfer, explicit data directories, and a 48-hour
feedback-triage SLA.

Bounded blob fetches and safe fetch destinations are resolved on the
pre-cohort-readiness branch. Network backpressure, concurrent connection/pipe
quotas, and duplicate-connection handling remain a mandatory gate before any
beta expansion beyond these three controlled attempts. This is an explicit
scope boundary, not risk acceptance.

## Pre-cohort readiness validation

The pre-cohort changes passed `scripts/verify.sh` and the complete
`scripts/release-readiness.sh` gate on 2026-07-10. All deterministic checks and
all serialized loopback tiers passed; the release gate reported
`release-readiness: READY`.

## Draft issue #100 update

> Pre-cohort infrastructure rehearsal completed on seven authorized Linux test
> hosts using the exact `v0.1.0-rc.1` source tag and a single verified release
> binary. This is not external-builder evidence and does not satisfy the human
> cohort acceptance criteria.
>
> PASS: two-person room join/message/verified-file flow; authenticated
> loopback-only Live Pipe HTTP flow plus direct-port non-exposure check; invited
> agent status with verified artifact retrieval. All long-running test processes
> shut down cleanly.
>
> GO for the first three supervised external attempts: the file recipe,
> loopback bind, exact-tag Linux source-build path, bounded blob retrieval, and
> atomic no-clobber destination handling are now addressed. Keep the network
> resource-limit gate visible before broader beta expansion, triage feedback
> within 48 hours, and do not close this issue until named external builders
> have completed the workflows.
