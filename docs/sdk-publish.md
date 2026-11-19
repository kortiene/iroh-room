# SDK crate publish procedure

This is the release procedure for the supported Rust SDK crates. It is a dry-run procedure. Nothing in this repository uploads a crate, logs into crates.io, or reads a registry token.

## Which crates

The SDK closure, in publish order. Git keeps `publish = false` on all four until the release pull request:

| Order | Package | Role |
| --- | --- | --- |
| 1 | `iroh-rooms-crypto` | SUITE_V1 content-key primitives. `iroh-rooms-core` depends on it. |
| 2 | `iroh-rooms-core` | Protocol, membership, tickets, optional store and sync. |
| 3 | `iroh-rooms-net` | Transport, blobs, and pipes. Optional dependency of the façade. |
| 4 | `iroh-rooms` | The supported SDK entry point. |

Not published (`publish = false`):

| Package | Why |
| --- | --- |
| `iroh-rooms-cli` | The binary is shipped as signed release archives, not as a library. |
| `iroh-rooms-v2-core` | v2 protocol core. It is not part of the v1 SDK closure and is not re-exported. |

Archive spikes under `archive/spikes/` are not workspace members and are not published.

## Versioning

The four SDK crates share one version. The current version is `0.1.0-rc.6`.

Pre-release by choice: Cargo recommends a library that depends on a pre-release (`ed25519-dalek` `=3.0.0-rc.0`) be a pre-release itself. Cargo does not reject a stable crate that depends on a pre-release. The version string stays `0.1.0-rc.6` until the maintainer chooses otherwise.

Within `0.x`, bump all four crates together:

- The stable façade (`identity`, `room`, `events`, `files`, `pipes`) changes only on a minor bump, with a `crates/iroh-rooms/CHANGELOG.md` entry.
- The `experimental` feature may change on any release.
- Path dependencies inside the workspace carry the same `version` as the package they point at. A version bump updates both sides.

`iroh-rooms-cli` and `iroh-rooms-v2-core` are not in this set. The CLI version tracks the binary release. `iroh-rooms-v2-core` stays at its own version until a later v2 decision.

## Ownership

crates.io ownership is held by the maintainers of `kortiene/iroh-room`. Adding or removing an owner is a manual `cargo owner` step run by a person who already has a crates.io token on their own machine.

This repository does not store that token, does not document a token value, and does not run `cargo owner` or `cargo login`.

## Publish order

Cargo 1.100 publishes the four crates in one invocation. The dry run checks them together against a local overlay, so each crate sees the others without those crates already being on crates.io. The package order inside that invocation is:

1. `iroh-rooms-crypto`
2. `iroh-rooms-core`
3. `iroh-rooms-net`
4. `iroh-rooms`

All four manifests in git keep `publish = false`. `scripts/verify.sh` fails if any of those lines is missing. Removing the lines is the real release pull request, and it waits for the maintainer.

The real upload is not part of this procedure and is not a workflow in this repository.

## Dry run

`scripts/publish-dry-run.sh` checks out `HEAD` into a temporary detached worktree, deletes the four `publish = false` lines there, commits that only inside the worktree, and runs:

```text
cargo publish --dry-run --locked --manifest-path <worktree>/Cargo.toml -p iroh-rooms-crypto -p iroh-rooms-core -p iroh-rooms-net -p iroh-rooms
```

There is no `--allow-dirty` and no `--no-verify`. A trap removes the worktree on success and on failure. The script refuses to start if its source contains `cargo login` or a `cargo publish` invocation that is not `--dry-run`. It does not read `CARGO_REGISTRY_TOKEN`.

`.github/workflows/sdk-publish-dry-run.yml` runs that script. It triggers only on `workflow_dispatch`. It has no registry secret and no upload step. Do not start that workflow from automation in this change.

## What this repository will not do

- No `cargo publish` without `--dry-run`.
- No `cargo login`.
- No crates.io token in the workflow, the script, or the docs.
- No git tag and no GitHub release from this procedure.
