# Changelog

All notable changes to the `iroh-rooms` SDK façade are documented here. See
`src/lib.rs` for the versioning policy: within `0.x`, the **stable** tier
changes only on a minor bump (with an entry here and a deprecation window
where feasible); the **experimental** tier may change on any release.

## Unreleased

- Added `examples/example_agent/` (issue #39 / IR-0304): a minimal, runnable
  example agent driven by real command-line arguments — the adapt-me-as-a-
  template evolution of `07_agent_status.rs` — plus a co-located `README.md`
  and a gated integration test. Docs-and-examples only; no SDK surface change.

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
