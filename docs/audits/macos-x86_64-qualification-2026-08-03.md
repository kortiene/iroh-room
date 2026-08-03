# macOS `x86_64-apple-darwin` qualification — 2026-08-03

## Scope

- **Platform under test:** native `x86_64-apple-darwin` (Intel Mac). This attests **only** `x86_64-apple-darwin`. **`aarch64-apple-darwin` remains untested** (no Apple Silicon in the fleet) — do not read this as covering both Apple triples.
- **Why:** `.github/workflows/verify.yml` runs `ubuntu-latest` only; both Apple triples shipped having never executed a test. This is the first native macOS qualification evidence in the repo.
- **Repository:** `kortiene/iroh-room`, branch `main`, SHA `edeae8d267f4d53d15bc69ea8cf1f6bb4e3eb8cc` (the merge of #220, which was required — see below).
- **Date:** 2026-08-03 07:59 EDT.
- **Method:** fresh qualification clone, `git pull --ff-only`, `ulimit -n 1024` (macOS defaults to 256 fds; the loopback integration tests hit `EMFILE` otherwise), `scripts/verify.sh`. No source edits / commits / pushes were performed during qualification; source worktree clean afterward.

## Host proof

| | |
|---|---|
| `uname -m` | `x86_64` |
| `uname -a` | `Darwin MacBookPro.lan 25.5.0 Darwin Kernel Version 25.5.0 … x86_64` |
| `sw_vers` | macOS 26.5.2 (Build 25F84) |
| `rustc -vV` | `rustc 1.96.0 …`; **`host: x86_64-apple-darwin`** (proves the run attests the requested triple) |
| Xcode CLT | `/Applications/Xcode.app/Contents/Developer` (C compiler for `rusqlite`'s `bundled` feature) |

## Verdict

**`x86_64-apple-darwin: QUALIFIED`** — `scripts/verify.sh` exited `0`.

All phases green:

- `cargo fmt --all --check` — PASS
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — PASS
- `cargo test --workspace --all-targets --all-features` — PASS
- `cargo test -p iroh-rooms --doc` — PASS (6)
- `cargo build -p iroh-rooms --examples` — PASS
- `cargo test -p iroh-rooms-crypto --doc` — PASS (1)

## Totals

**2505 passed; 0 failed; 20 ignored** across all `verify.sh` phases (2498 in the workspace `--all-targets --all-features` run across 102 test binaries + 7 doctests). The 20 ignored are the loopback `#[ignore]`-gated online tiers (run separately by `release-readiness.sh`), unchanged from ubuntu.

No `rusqlite`/`libsqlite3-sys`, `EMFILE`, kqueue, tokio, `getrandom`, or firewall-related failure.

## One defect found during qualification — fixed in #220

The first run (2026-08-02, SHA `3acf956`) was NOT QUALIFIED: two `spike-N40` `self_check` tests (`n5_metrics_render_markdown_and_json_without_panic`, `n5_disconnect_peer_drops_then_redials_and_delivery_recovers`) panicked because they `.expect()`-ed `process_rss_bytes()`, which reads Linux-only `/proc/self/status` (`crates/spike-N40/src/rss.rs` is Linux-only by design). All 2370 **shipping-crate** tests passed even on that first run — the platform hole was, in practice, a single spike-crate harness bug.

**Fix (#220, merged `edeae8d`):** tolerate non-Linux at the two RSS call sites (`self_check.rs` baseline + `cluster_metrics` per-sample) via `.unwrap_or(0)`; the shipping measurement binary (`main.rs`) is deliberately left fail-closed on non-Linux to preserve the no-fabricated-number stance for release measurement. Linux unchanged. The 2026-08-03 re-run at `edeae8d` is green.

## Per-binary results (condensed; cargo target hashes stripped)

Every emitted `test result:` line was `ok`. Format: `binary — N passed; 0 failed; M ignored`.

**iroh-rooms (façade):** `src/lib.rs` 0/0 · `tests/example_agent_e2e` 9/3 · `tests/experimental_surface` 11/0 · `tests/facade_e2e` 4/0 · `tests/iroh_pin_consistency` 2/0 · `tests/stable_surface` 11/0 · `tests/store_concurrency_e2e` 2/0 · `src/main.rs` (CLI) 286/0 · 8 examples 0/0

**iroh-rooms-cli:** `tests/agent_cli` 24/0 · `tests/agent_e2e` 0/2 · `tests/agent_invite_flow` 8/0 · `tests/diagnostics_cli` 4/0 · `tests/docs_conformance` 74/0 · `tests/error_taxonomy` 26/0 · `tests/error_taxonomy_e2e` 2/1 · `tests/file_cli` 42/0 · `tests/full_demo_e2e` 12/6 · `tests/identity_cli` 37/0 · `tests/invite_cli` 25/0 · `tests/join_cli` 12/0 · `tests/live_pipe_preview_docs` 23/0 · `tests/message_cli` 14/0 · `tests/no_direct_iroh_dep` 3/0 · `tests/phase0_memo_conformance` 20/0 · `tests/pipe_cli` 19/1 · `tests/release_readiness_docs` 19/0 · `tests/release_readiness_e2e` 6/0 · `tests/room_cli` 40/0 · `tests/tail_cli` 45/0 · `tests/two_peer_e2e` 18/5

**iroh-rooms-core:** `src/lib.rs` 300/0 · `tests/cbor_property` 6/0 · `tests/compatibility` 7/1 · `tests/e2e_lifecycle` 12/0 · `tests/encrypted_authz_reads` 11/0 · `tests/encrypted_envelope` 15/0 · `tests/encrypted_write_path` 19/0 · `tests/file_shared_hashes` 5/0 · `tests/golden_vectors` 58/0 · `tests/membership_fold` 32/0 · `tests/membership_store_e2e` 6/0 · `tests/protocol_conformance` 81/1 · `tests/store_e2e` 5/0 · `tests/sync_batch_dedup_e2e` 5/0 · `tests/sync_convergence` 46/0 (67s) · `tests/sync_restart` 16/0 · `tests/sync_smoke` 43/0

**iroh-rooms-crypto:** `tests/banned_dependencies` 1/0 · `tests/suite_v1` 13/0

**iroh-rooms-net:** `src/lib.rs` 269/0 · `src/bin/net_smoke` 0/0 · `tests/approach_to_ceiling_e2e` 3/0 · `tests/blob_e2e` 5/0 · `tests/blob_import` 5/0 · `tests/blob_import_live_e2e` 2/0 · `tests/file_e2e` 3/0 · `tests/frame` 9/0 · `tests/join_e2e` 10/0 · `tests/loopback` 9/0 · `tests/malformed_cbor_e2e` 2/0 · `tests/manager` 12/0 · `tests/manager_e2e` 5/0 · `tests/membership_projection_cache_e2e` 2/0 · `tests/message_e2e` 8/0 · `tests/pipe_e2e` 13/0 · `tests/queue_budget_e2e` 3/0 · `tests/room_events_e2e` 2/0

**iroh-rooms-v2-core:** `src/lib.rs` 275/0 · `tests/banned_dependencies` 1/0 · `tests/content_body_validation` 7/0 · `tests/governance_state_machine` 15/0 · `tests/identifiers` 15/0 · `tests/member_sorted_merkle` 18/0 · `tests/room_lifecycle` 4/0 · `tests/signed_records_golden` 35/0 · `tests/taxonomy` 3/0 · `tests/v2_governance_fork_e2e` 9/0 · `tests/v2_governance_log_e2e` 16/0 · `tests/v2_identifiers_e2e` 14/0

**Spikes:** `spike-blobs` lib 38/0 + `tests/blob_acl` 11/0 · `spike-N40` lib 47/0 + `n40_probe` main 14/0 + **`tests/self_check` 9/0 (14.7s — the two previously-failing tests now green)** · `spike-nat` lib 41/0 + `nat_probe` main 22/0 + `tests/self_check` 13/0 · `spike-transport` lib 26/0 + `transport_probe` main 10/0 + `tests/cluster` 5/0 + `tests/self_check` 9/0

**Doctests:** `iroh-rooms` 6/0 · `iroh-rooms-crypto` 1/0

(`member_merkle` bench ran and emitted its record; cargo emits no `test result:` line for it.)

## Caveats

- **`aarch64-apple-darwin` is NOT covered** — no Apple Silicon in the fleet. That triple continues to ship untested.
- **Point-in-time:** this attests SHA `edeae8d` only. It is a manual qualification, **not** a CI gate — `verify.yml` still runs `ubuntu-latest` only, so a future macOS regression would not be caught until a re-run. Follow-up: add `macos-13` (x86_64) to the `verify.yml` / `msrv.yml` matrix so the qualification self-renews.
- The optional heavier gates (`release-readiness.sh` loopback online tiers; the 1.91/1.85 MSRV checks) were **not** run on macOS — they are loopback and very likely fine, but unmeasured. Worth adding to a future macOS matrix run.

## What this attests

The iroh-room workspace builds and its full deterministic test suite passes natively on `x86_64-apple-darwin` (fmt, clippy `-D warnings`, every workspace test binary, doctests, examples) at `edeae8d`. This closes the Intel macOS coverage gap; the residual Apple triple (`aarch64-apple-darwin`) remains open.
