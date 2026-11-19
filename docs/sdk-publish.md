# SDK crate publish procedure

This is the release procedure for the supported Rust SDK crates. It is a dry-run procedure. Nothing in this repository uploads a crate, logs into crates.io, or reads a registry token.

## Which crates

Published, in this order, because each crate's public types are part of the next crate's API:

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

The four published crates share one version. The current version is `0.1.0-rc.6`.

That version is a pre-release on purpose. `iroh-rooms-crypto` and `iroh-rooms-core` depend on `ed25519-dalek` `=3.0.0-rc.0`. crates.io rejects a non-pre-release crate that depends on a pre-release. Do not drop the `-rc` suffix until that pin is a stable release.

Within `0.x`, bump all four crates together:

- The stable façade (`identity`, `room`, `events`, `files`, `pipes`) changes only on a minor bump, with a `crates/iroh-rooms/CHANGELOG.md` entry.
- The `experimental` feature may change on any release.
- Path dependencies inside the workspace carry the same `version` as the package they point at. A version bump updates both sides.

`iroh-rooms-cli` and `iroh-rooms-v2-core` are not in this set. The CLI version tracks the binary release. `iroh-rooms-v2-core` stays at its own version until a later v2 decision.

## Ownership

crates.io ownership is held by the maintainers of `kortiene/iroh-room`. Adding or removing an owner is a manual `cargo owner` step run by a person who already has a crates.io token on their own machine.

This repository does not store that token, does not document a token value, and does not run `cargo owner` or `cargo login`.

## Publish order

Publish one crate, wait until it is visible on crates.io, then publish the next:

1. `iroh-rooms-crypto`
2. `iroh-rooms-core`
3. `iroh-rooms-net`
4. `iroh-rooms`

A dependent crate's `cargo publish --dry-run` and `cargo package` both look the path dependency up on crates.io by version. Until that dependency has actually been published, both commands fail with "no matching package". That failure does not ask for a registry token and it is not a reason to publish. The leaf crate, `iroh-rooms-crypto`, has no path dependency, so its dry run is the check that the manifest metadata packages. After `iroh-rooms-crypto` is on crates.io, the same check moves to `iroh-rooms-core`, and so on.

The real upload is not part of this procedure and is not a workflow in this repository. A maintainer runs it by hand, from a clean tag, after the dry run for that crate has succeeded against the registry.

## Dry run

`scripts/publish-dry-run.sh` runs, in the order above, for each crate:

- `cargo publish --dry-run --locked --allow-dirty --manifest-path <crate>/Cargo.toml`
- `cargo package --locked --allow-dirty --no-verify --manifest-path <crate>/Cargo.toml`

It keeps going after a non-zero exit so every crate is attempted, then exits non-zero if any command did. The script refuses to start if its source contains `cargo login` or a `cargo publish` invocation that is not `--dry-run`. It does not read `CARGO_REGISTRY_TOKEN`.

`.github/workflows/sdk-publish-dry-run.yml` runs that script. It triggers only on `workflow_dispatch`. It has no registry secret and no upload step. Do not start that workflow from automation in this change.

## What this repository will not do

- No `cargo publish` without `--dry-run`.
- No `cargo login`.
- No crates.io token in the workflow, the script, or the docs.
- No git tag and no GitHub release from this procedure.
