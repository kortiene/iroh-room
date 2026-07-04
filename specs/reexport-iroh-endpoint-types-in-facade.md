# Re-export the iroh endpoint types the public session API requires

**Issue:** #87 — `feat(sdk): re-export the iroh endpoint types the public session
API requires (EndpointAddr, EndpointId, SecretKey)`
**Labels:** `enhancement`, `type/feature`
**Status:** planning / spec only (no production code in this change)
**Owning crate:** `crates/iroh-rooms` (the SDK façade, IR-0301); reference
consumer `crates/iroh-rooms-cli`

---

## 1. Problem

The façade's design promise (spec `initial-rust-sdk-surface.md`, `src/lib.rs`) is
that a Rust consumer imports **only** through `iroh_rooms::*` paths: "A re-exported
type is *the same type* as its `core`/`net` original … so mixing this façade with a
direct `core`/`net` dependency never produces two incompatible copies of the same
type."

That promise holds everywhere except the online session boundary. Driving the
experimental runtime requires naming three raw `iroh` types in consumer
signatures, and the façade re-exports none of them (there is no `pub use iroh::…`
anywhere in `crates/iroh-rooms/src/`). Verified public signatures on
`iroh_rooms_net::Node` (`crates/iroh-rooms-net/src/node.rs`) that force the leak:

| Type | Public API surface that names it | Location |
|---|---|---|
| `SecretKey` | `Node::spawn`, `Node::spawn_with_pipe_audit`, `Node::spawn_room` (param) | `node.rs:211,233,280` |
| `EndpointAddr` | `Node::spawn_room` (param), `Node::endpoint_addr` (return), `Node::connect_to`, `Node::pipe_connect`, `Node::fetch_file` (param) | `node.rs:286,456,461,830,892` |
| `EndpointId` | `Node::id` (return), `disconnect_peer`, `peer_state`, `peer_states`, `peer_entries`, `wait_for_state`, `peer_paths`; also `BlobAclView::is_active`, `PipeSessionInfo.device`, every `AuditSink`/`PipeAuditSink` callback | `node.rs:448,466,472,478,486,537,652`; `blob/mod.rs:375`; `audit.rs:49–55`; pipe audit |
| `Endpoint` (candidate) | `Node::endpoint` (return) | `node.rs:853` |

Because `iroh-rooms-net` pins `iroh = "=1.0.1"`
(`crates/iroh-rooms-net/Cargo.toml:37`), every downstream project must add its
**own** direct `iroh` dependency and keep it byte-identical to that pin. If it
drifts, Cargo resolves two `iroh` crates and the consumer's `EndpointAddr` becomes
a *different* type from the SDK's — a silent compile-error trap that every
downstream (the filing consumer **Bantaba**, rev `1d2f014`) rediscovers.

The reference CLI itself takes the leak today:

- `crates/iroh-rooms-cli/src/file.rs:31` — `use iroh::{EndpointAddr, EndpointId, SecretKey};`
- `crates/iroh-rooms-cli/src/join.rs:38`, `src/message.rs:34`, `src/pipe.rs:17`, `src/audit.rs:14`

and carries a matching `iroh = "=1.0.1"` in `crates/iroh-rooms-cli/Cargo.toml`.
The façade e2e test documents the gap in prose
(`crates/iroh-rooms/tests/facade_e2e.rs:16–21`): "`iroh::{SecretKey, EndpointId}`
are still imported directly: the façade does not (yet) wrap the raw transport
identity/dial primitives (spec OQ5)."

## 2. Goal

Follow the façade's existing **same-type re-export** policy so that:

1. The three transport types (and, by decision below, `Endpoint`) are reachable
   through `iroh_rooms::experimental::*` paths.
2. A consumer crate — the CLI is the proving candidate — compiles with its direct
   `iroh` dependency **deleted**.
3. The `iroh` pin becomes an SDK-internal detail, exactly as the façade already
   treats `iroh-rooms-core` / `iroh-rooms-net`.
4. `docs/sdk-coverage.md` gains rows for the new re-exports and the "imports only
   through the façade" claim becomes fully true for online consumers.

Non-goal: **wrapping** the iroh types (new ergonomic newtypes) — that is the
separate OQ5 follow-up. This issue re-exports the *same* types verbatim, which is
what preserves type-identity across the façade/net boundary.

## 3. Design

### 3.1 What re-exports go where

The types live in the `experimental` tier only (they name IO-bearing transport
identities and have no meaning in the stable/pure tier), so all re-exports are
behind the existing `experimental` cargo feature.

**Canonical home — `crates/iroh-rooms/src/experimental/session.rs`** (matches the
issue's suggested shape and is where `Node`/`spawn*` already live):

```rust
pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
```

**Submodule duplication for self-containment** — `EndpointId` also appears in
`blob` and `pipe_runtime` public signatures. Mirror the façade's existing
duplicate-where-used precedent (`RoomId` is re-exported from both `room` and
`events`; `HashRef` from both `files` and `events`) so a consumer working only
with blobs or pipes need not reach into `session`:

- `crates/iroh-rooms/src/experimental/blob.rs` — add `pub use iroh::EndpointId;`
  (named by `BlobAclView::is_active(EndpointId)`).
- `crates/iroh-rooms/src/experimental/pipe_runtime.rs` — add
  `pub use iroh::EndpointId;` (named by `PipeSessionInfo.device` and the
  `PipeAuditSink` callbacks).

`EndpointAddr` and `SecretKey` are session-only concepts, so they are **not**
duplicated into `blob`/`pipe_runtime`.

Every re-export must sit under the `experimental` feature gate (the whole
`experimental` module already is) and carry the module's existing
`Experimental (unstable API).` framing — see §3.4 for the docs the re-export
needs.

### 3.2 Make `iroh` a direct, feature-gated dependency of the façade

`pub use iroh::…` requires `iroh` to be a **direct** dependency of
`iroh-rooms` — it is currently only a `[dev-dependencies]` entry
(`crates/iroh-rooms/Cargo.toml:45`, used by the examples). Add it as an optional
dependency wired to the `experimental` feature so a default-features build still
cannot name it:

```toml
[dependencies]
# Re-exported verbatim from `experimental::session` (issue #87): EndpointAddr /
# EndpointId / SecretKey / Endpoint are the transport identities the online
# `Node` API names. Same-type re-export — the pin MUST match `iroh-rooms-net`'s
# so Cargo unifies to one `iroh` crate and the re-exported types are identical to
# net's. Optional + experimental-gated: a default-features build cannot name it.
iroh = { version = "=1.0.1", optional = true }

[features]
experimental = ["dep:iroh-rooms-net", "iroh-rooms-core/sync", "dep:iroh"]
```

- The `dev-dependencies` `iroh = "=1.0.1"` line becomes redundant once the
  examples/tests route through the façade (§4.4) — remove it, or keep it only if
  a residual example still imports `iroh` directly. Prefer removing it so the
  façade crate practices its own policy.
- **Pin-identity is the load-bearing correctness constraint.** The pin must equal
  `iroh-rooms-net`'s exactly (`=1.0.1`). Because both are the identical exact
  version, Cargo unifies them into a single crate instance and the re-export is
  the same `TypeId` net uses. Any drift reintroduces the two-crates bug this
  issue exists to kill.

### 3.3 Drift-proofing (recommended)

There is no `[workspace.dependencies]` today. To make future iroh bumps a
one-line change that cannot desync `-net` and the façade, hoist the pin:

```toml
# root Cargo.toml
[workspace.dependencies]
iroh = "=1.0.1"
```
then `iroh = { workspace = true, optional = true }` in the façade and
`iroh = { workspace = true }` in `iroh-rooms-net` (and the CLI's dev use, if any
remains). This is optional but strongly recommended — it converts an invariant
that is currently enforced by hand (three copies of `=1.0.1`) into one the build
enforces. If deferred, add a code-comment cross-reference in both Cargo.tomls
pointing at the other pin.

### 3.4 Docs & lints

- `#![deny(missing_docs)]` is active (`src/lib.rs:80`). A simple `pub use` of an
  external item does **not** require its own doc comment (the existing
  `pub use iroh_rooms_net::{…}` in `session.rs` compiles fine under the deny), so
  no per-symbol docs are needed. Keep/extend the module-level `//!` doc to name
  the newly re-exported types and state the stability caveat: **the re-exported
  iroh types track the pinned `iroh` release and may change when that pin moves**
  (they inherit the experimental tier's "may change on any release" promise).
- Update `session.rs`'s existing module doc (which already explains why `tokio`
  and `bytes` are *not* re-exported) to add: "The transport identities
  `EndpointAddr` / `EndpointId` / `SecretKey` (and `Endpoint`) *are* re-exported
  here — a consumer needs no direct `iroh` dependency to drive the online API."
- No `doc(cfg)` change needed — the `experimental` module already carries the
  badge.

## 4. Implementation steps

### 4.1 Façade re-exports (production code — the feature itself)
1. `crates/iroh-rooms/src/experimental/session.rs`: add
   `pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};` and extend the
   module doc per §3.4.
2. `crates/iroh-rooms/src/experimental/blob.rs`: add `pub use iroh::EndpointId;`.
3. `crates/iroh-rooms/src/experimental/pipe_runtime.rs`: add
   `pub use iroh::EndpointId;`.
4. `crates/iroh-rooms/Cargo.toml`: add the optional `iroh` dependency and the
   `dep:iroh` entry on the `experimental` feature (§3.2); remove the now-redundant
   dev-dependency `iroh` line if §4.4 removes the last direct example/test use.
5. (Recommended) root `Cargo.toml`: add `[workspace.dependencies] iroh = "=1.0.1"`
   and switch `-net` + façade to `workspace = true` (§3.3).

### 4.2 Prove it with the reference consumer — delete the CLI's direct `iroh` dep
This is the AC-proving change. Every `iroh::` reference in the CLI (both `use`
statements **and** inline fully-qualified paths) must route through the façade.

Enumerated sites (production):
- `file.rs:31` `use iroh::{EndpointAddr, EndpointId, SecretKey};` →
  `use iroh_rooms::experimental::session::{EndpointAddr, EndpointId, SecretKey};`
- `join.rs:38`, `message.rs:34`, `pipe.rs:17` — same swap (each imports the trio).
- `audit.rs:14` `use iroh::EndpointId;` →
  `use iroh_rooms::experimental::session::EndpointId;`
- Inline fully-qualified production refs (grep `iroh::` — not just `use iroh`):
  - `pipe.rs:526` `iroh::EndpointId::from_bytes(…)` → `EndpointId::from_bytes(…)`
    (add to the file's façade import) or `iroh_rooms::experimental::session::EndpointId::from_bytes`.

Then delete `iroh = "=1.0.1"` from `crates/iroh-rooms-cli/Cargo.toml`
`[dependencies]` and update the accompanying comment.

Notes:
- All methods the CLI calls (`EndpointAddr::new`, `.with_ip_addr`, `.id` field,
  `EndpointId::from_bytes`, `EndpointId::from_str`, `SecretKey::from_bytes`,
  `.public()`) are inherent methods/fields on the re-exported types — re-exporting
  the types is provably sufficient; the CLI's *only* production iroh imports are
  the trio, so no additional iroh symbol is required.
- `SocketAddr` in `.with_ip_addr(sock)` is `std::net::SocketAddr`, not an iroh
  type — no extra re-export needed.

### 4.3 Migrate the CLI's test modules (required for a *full* deletion)
Deleting the `iroh` dependency (not just moving it to dev) means `#[cfg(test)]`
imports must also route through the façade (the CLI already enables
`iroh-rooms/experimental`, so the paths resolve in tests):
- `file.rs:1048`, `join.rs:440`, `message.rs:1235,1310`, `pipe.rs:770,958,981,1082`,
  `audit.rs:91,97` — swap `use iroh::{…}` / `iroh::EndpointId` for the
  `iroh_rooms::experimental::session::{…}` equivalents.
- If any single test genuinely needs an iroh symbol the façade does **not**
  re-export, prefer adding that symbol to the façade over keeping a dev-only
  `iroh` dependency (keeps the "no direct iroh dep" property exact). None is
  expected — the test modules use only the trio + `.public()`.

### 4.4 (Optional, recommended) Practice the policy inside the façade crate
The façade's own examples/tests still import `iroh` directly and could adopt the
re-export, letting `crates/iroh-rooms/Cargo.toml` drop its dev-dependency `iroh`:
- `examples/03_invite_and_join.rs:25`, `04_send_message.rs:19`,
  `05_share_and_fetch_file.rs:20`, `06_live_pipe.rs:15`, `07_agent_status.rs:16`,
  `examples/example_agent/main.rs:47` — swap to
  `use iroh_rooms::experimental::session::{EndpointAddr, EndpointId, SecretKey};`.
- `tests/facade_e2e.rs:33` and `tests/example_agent_e2e.rs:557,727,731` — swap,
  and update the `facade_e2e.rs:16–21` prose that currently explains *why* iroh is
  imported directly (that caveat is now resolved). **Migrating `facade_e2e.rs`
  doubles as the type-identity regression test** (see §6): it passes a re-exported
  `SecretKey` into `Node::spawn` and a re-exported `EndpointId`/`EndpointAddr`
  through the live path — if the façade's iroh ever desynced from net's, this test
  would fail to compile.

Scope note: §4.4 is inside the façade crate, so it does not by itself prove the
downstream-consumer AC (§4.2 does). Include it to make the façade eat its own dog
food and to install the regression guard; it is safe to land in the same change.

### 4.5 Update `docs/sdk-coverage.md`
- Add a new subsection under **Experimental tier** — e.g. "iroh transport types":

  | `iroh` symbol | Façade path | Used by |
  |---|---|---|
  | `EndpointAddr` | `experimental::session::EndpointAddr` | file.rs, join.rs, message.rs, pipe.rs |
  | `EndpointId` | `experimental::session::EndpointId` (also `blob::EndpointId`, `pipe_runtime::EndpointId`) | audit.rs, file.rs, join.rs, message.rs, pipe.rs |
  | `SecretKey` | `experimental::session::SecretKey` | file.rs, join.rs, message.rs, pipe.rs |
  | `Endpoint` | `experimental::session::Endpoint` | (none in CLI; completeness) |
- Remove/replace the `facade_e2e.rs`-style "still imported directly" caveat
  wherever the coverage narrative repeats it.
- Extend the **Method** grep to include `use iroh::` and inline `iroh::` so the
  audit proves zero direct-`iroh` residue in CLI production code:
  ```console
  grep -n "use iroh_rooms_core::\|use iroh_rooms_net::\|use iroh_rooms::\|iroh::" crates/iroh-rooms-cli/src/*.rs
  ```
  (After migration this should surface only `iroh_rooms::` paths in production.)
- Update the closing narrative: the CLI's `Cargo.toml` no longer carries a direct
  `iroh` dependency; the "imports only through the façade" claim is now fully true
  for the online tier.

### 4.6 Optional doc/README touch-ups
- `crates/iroh-rooms/src/lib.rs` "Getting started" / OQ5 mentions and
  `docs/live-pipe-preview.md` / `docs/getting-started.md` if they instruct
  consumers to add a direct `iroh` dependency — update to point at the re-exports.
  (Verify with a grep for `iroh = "=1.0.1"` / "add iroh" in `docs/` before
  editing.)

## 5. Decisions

- **D1 — Re-export, do not wrap.** Preserve type-identity by re-exporting the same
  `iroh` types. Newtype wrappers (OQ5) are explicitly out of scope; wrapping would
  *break* the same-type property this issue depends on.
- **D2 — Canonical location = `experimental::session`**, matching the issue's
  suggested shape and where `Node`/`spawn*` live. `EndpointId` additionally
  duplicated into `blob` and `pipe_runtime` (RoomId/HashRef precedent) for
  submodule self-containment.
- **D3 — Include `Endpoint`.** `Node::endpoint() -> iroh::Endpoint` is public, so
  strictly "any iroh type a public session signature requires" includes it.
  Re-exporting it makes the façade-only claim exact. It is *not* needed by the CLI
  (no `.endpoint()` call), and it exposes a lower-level handle — see OQ1 for the
  trade-off if a reviewer prefers a minimal surface.
- **D4 — Feature-gated + optional dep.** `dep:iroh` on `experimental` keeps a
  default-features build unable to name the types, preserving the stable-tier
  purity invariant (spec D4).
- **D5 — Full deletion of the CLI's iroh dep** (dependency *and* dev-dependency),
  which requires migrating test-module imports too — a stronger, cleaner proof of
  the AC than merely demoting it to a dev-dependency.
- **D6 — Recommend hoisting the pin to `[workspace.dependencies]`** to make
  pin-drift structurally impossible; acceptable to defer with a cross-reference
  comment.

## 6. Test strategy

No new runtime behavior — this is a re-export + import-routing change. Coverage:

1. **Compile-is-the-test (primary AC).** `crates/iroh-rooms-cli` builds and its
   full test suite passes with **no** `iroh` entry in its `Cargo.toml`. Run
   `cargo build -p iroh-rooms-cli` and `cargo test -p iroh-rooms-cli`.
2. **Type-identity regression guard.** Migrating `tests/facade_e2e.rs` (§4.4) to
   consume the re-exported trio and feed it into the real loopback-QUIC `Node`
   path is the guard: it only compiles if the façade's `iroh` is the *same* crate
   instance `iroh-rooms-net` uses. Keep this test in the CI (non-`#[ignore]`)
   tier. Optionally add a tiny explicit assertion in `experimental_surface.rs`
   that the re-export paths resolve (mirrors the existing surface-resolution
   tests).
3. **Whole-workspace gate.** `scripts/verify.sh` (the real CI gate — it runs
   `fmt --check` + `clippy -D warnings` pedantic + `--all-features` tests, per the
   project's `verify-sh-is-the-real-ci-gate` note). New `pub use` lines and the
   `Cargo.toml` edits must pass fmt/clippy; the `--all-features` run exercises
   `facade_e2e.rs`.
4. **Docs audit self-check.** Re-run the updated `docs/sdk-coverage.md` "Method"
   grep and confirm zero direct-`iroh` production imports remain in the CLI.

## 7. Acceptance criteria

- **AC1** — `iroh_rooms::experimental::session` re-exports `EndpointAddr`,
  `EndpointId`, `SecretKey` (and `Endpoint`, per D3); `experimental::blob` and
  `experimental::pipe_runtime` re-export `EndpointId`. All behind the
  `experimental` feature; a default-features build cannot name them.
- **AC2** — `crates/iroh-rooms-cli` compiles and all its tests pass with the direct
  `iroh` dependency deleted from `crates/iroh-rooms-cli/Cargo.toml` (both
  `[dependencies]` and, if present, `[dev-dependencies]`); no `iroh::` path remains
  in CLI production source.
- **AC3** — The re-exported types are provably the *same* types as
  `iroh-rooms-net`'s (demonstrated by `facade_e2e.rs` driving a live `Node` with
  façade-imported `SecretKey`/`EndpointAddr`/`EndpointId`).
- **AC4** — `docs/sdk-coverage.md` gains the iroh-transport-types rows, updates the
  Method grep, and its "imports only through the façade" claim is stated as fully
  true for online consumers.
- **AC5** — `scripts/verify.sh` passes (fmt + clippy pedantic + `--all-features`
  tests), and the façade `Cargo.toml` gates `iroh` under `experimental`
  (`dep:iroh`).

## 8. Risks & mitigations

- **R1 — Pin drift reintroduces the two-crate bug.** If the façade's `iroh` pin
  ever diverges from `iroh-rooms-net`'s, the re-exports become a *different*
  `EndpointAddr` and downstream code breaks with confusing "expected X, found X"
  errors. *Mitigation:* exact `=1.0.1` matching net; strongly prefer the
  `[workspace.dependencies]` hoist (D6); the `facade_e2e.rs` compile-guard (§6.2)
  catches an in-tree desync immediately.
- **R2 — Coupling the experimental API to iroh's semver.** Re-exporting raw iroh
  types means an iroh bump can change the façade's experimental surface.
  *Mitigation:* this is the *experimental* tier, already documented as "may change
  on any release"; make the doc say the re-exports track the pinned iroh. The
  stable tier is untouched.
- **R3 — Surface widening via `Endpoint` (D3).** `Endpoint` is a lower-level handle
  than a consumer usually needs. *Mitigation:* it is only reachable in the
  experimental tier and only because `Node::endpoint()` already returns it; drop it
  if a reviewer prefers a minimal surface (OQ1) — the three core types satisfy the
  CLI AC without it.
- **R4 — Incomplete migration leaves a stray `iroh::`.** A missed inline
  fully-qualified `iroh::EndpointId::…` (e.g. `pipe.rs:526`) will fail to compile
  once the dep is gone — caught immediately by `cargo build`, but call it out so
  the implementer greps `iroh::`, not just `use iroh`.
- **R5 — `deny(missing_docs)` surprise.** Some engineers expect `pub use` to
  require docs. It does not for external re-exports (the existing
  `pub use iroh_rooms_net::{…}` proves it); noted so nobody adds spurious doc
  scaffolding.
- **R6 — Test-module deletion friction.** Fully removing the dep (vs. demoting to
  dev) forces the `#[cfg(test)]` imports to migrate too. *Mitigation:* enumerated
  in §4.3; the façade already enables `experimental` for the CLI so the paths
  resolve.

## 9. Open questions

- **OQ1 — Include `Endpoint`?** Recommended (D3) for a fully-honest
  façade-only claim, but the CLI doesn't need it and it exposes a low-level
  handle. Ship the trio only if minimal-surface is preferred; `Endpoint` can be
  added later without a breaking change.
- **OQ2 — Hoist the pin to `[workspace.dependencies]` now, or defer?** Recommended
  now (D6); deferring is acceptable with a cross-reference comment in both
  Cargo.tomls.
- **OQ3 — Migrate the façade's own examples/tests (§4.4) in this change, or a
  follow-up?** Recommended together (it installs the type-identity guard and lets
  the crate drop its dev-dependency `iroh`), but it is separable from the
  downstream-consumer AC.
- **OQ4 — Any other iroh type on a *future* public signature?** Today the trio
  (+`Endpoint`) fully covers `session`/`blob`/`pipe_runtime`. If a later online
  method adds, e.g., `RelayUrl` to a public signature, extend the same
  `session.rs` re-export block; this spec establishes that pattern.
- **OQ5 — Ergonomic newtype wrappers.** Explicitly out of scope here (this is a
  re-export, not a wrap). Track the wrapper question as the pre-existing OQ5
  follow-up referenced in `facade_e2e.rs`.
