#!/usr/bin/env bash
set -euo pipefail

# sign-and-publish's environment: and RELEASE_ENVIRONMENT must be the same
# name. The check does not decide what that name is.
scripts/check-release-environment.sh

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
# Shipping config: default features, gossip_overlay off. The --all-features
# run above compiles the gossip-only helpers, so it does not catch dead code
# on the default build. A failure here fails the required verify check.
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --all-features

# `cargo test --all-targets` does not run doctests; the SDK façade's stable
# module docs carry compilable examples (spec IR-0301 AC1) that only
# `--doc` exercises.
cargo test -p iroh-rooms --doc

# The SDK façade's examples must compile in both feature configurations: the
# default (offline) tier, and every example (including the online ones)
# under `--features experimental` (spec IR-0301 §6 step 10 / L3). The
# `--all-features` test run above already covers the experimental config as
# a side effect of `--all-targets`; the default-features build below is the
# one config that run does not exercise.
cargo build -p iroh-rooms --examples


# The pure crypto crate's lib docs carry a compilable wrap/unwrap example
# (#191 step 2 acceptance: doctests green); only `--doc` exercises it, and
# the `--all-targets` workspace run above skips doctests entirely.
cargo test -p iroh-rooms-crypto --doc

# Local lifecycle-smoke harness. The networked install is lifecycle-smoke.sh
# itself and is not run here.
scripts/lifecycle-smoke-test.sh
