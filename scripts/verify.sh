#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
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

