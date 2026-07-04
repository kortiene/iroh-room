# Changelog

All notable changes to the `iroh-rooms` SDK façade are documented here. See
`src/lib.rs` for the versioning policy: within `0.x`, the **stable** tier
changes only on a minor bump (with an entry here and a deprecation window
where feasible); the **experimental** tier may change on any release.

## Unreleased

- Added `Node::room_events() -> broadcast::Receiver<StoredEvent>` (issue #83 /
  IR-0307, `experimental::session`): a live push stream of every event accepted
  into the room's store — own publish, peer sync, and delayed park-promotion
  all emit here exactly once, so a long-running consumer (e.g. a resident
  daemon driving a UI) no longer has to poll `room_tail`. Lossy on lag like
  `conn_events` (`RecvError::Lagged`, resync via `room_tail` + a seen-set —
  see the method's doc comment for the recipe). Purely additive; existing
  `Node` methods are unchanged.
- Added `examples/example_agent/` (issue #39 / IR-0304): a minimal, runnable
  example agent driven by real command-line arguments — the adapt-me-as-a-
  template evolution of `07_agent_status.rs` — plus a co-located `README.md`
  and a gated integration test. Docs-and-examples only; no SDK surface change.
- Added `JoinBootstrapAdmission::new_dynamic` (issue #88, `experimental::session`):
  the join-bootstrap window (`accept_joins`) can now be read from a shared
  `Arc<AtomicBool>` on every `authorize()` call instead of being fixed at
  construction, so a long-running host (e.g. a resident daemon) can gate
  provisional admission on pending invites without respawning its `Node`.
  Purely additive — `new` and its fixed-`bool` semantics are unchanged, and
  `new_dynamic` is observationally identical to `new` for any fixed flag
  value.

## 0.1.0 — initial surface (IR-0301)

Initial developer-preview release. Defines the SDK boundary:

- Five stable domain modules — `identity`, `room`, `events`, `files`, `pipes`
  — re-exporting the deterministic, conformance-tested protocol layer from
  `iroh-rooms-core` (event authoring/validation, the membership fold, the
  invite ticket codec).
- An `experimental` cargo feature gating the online runtime — `session`
  (transport/admission/connection state), `sync` (the sans-IO engine), `store`
  (the local event store), `blob` (import/serve/fetch), `pipe_runtime`
  (live-pipe forwarding) — re-exported from `iroh-rooms-net` /
  `iroh-rooms-core`.
- A `prelude` module glob-re-exporting the most-used stable types.
- `examples/` mirroring the `docs/getting-started.md` demo, plus doctests on
  every stable module.
- The CLI (`iroh-rooms-cli`) migrated its offline authoring path
  (`identity`, `room` create/members, `invite`, and the `build_*` call sites
  in `message`/`file`) to import through this façade — see
  `docs/sdk-coverage.md` for the full coverage audit.

No crates.io publication yet (`publish = false`); no stability guarantee on
the `experimental` tier.
