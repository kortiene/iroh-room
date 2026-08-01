//! Banned-dependency guard, mirroring `iroh-rooms-v2-core`'s purity
//! tripwire (spec `content-key-rotation.md` D3: the `SUITE_V1` primitives live
//! in a pure, deterministic, sans-IO crate — same invariants as v2-core).
//!
//! Asserts that `iroh-rooms-crypto`'s dependency tree contains NONE of the
//! runtime/store/network crates the pure-crate invariants forbid: `iroh`,
//! `iroh-blobs`, `iroh-gossip`, `tokio`, `rusqlite`.
//!
//! This runs `cargo tree -p iroh-rooms-crypto` at test time (cargo is
//! available in CI; the v2-core guard uses the same mechanism).

use std::process::Command;

/// Crate names the pure crypto crate must NEVER depend on, directly or
/// transitively.
const BANNED: &[&str] = &["iroh", "iroh-blobs", "iroh-gossip", "tokio", "rusqlite"];

#[test]
fn dependency_tree_contains_no_banned_crates() {
    // `cargo tree` must run against the package's own workspace. Use the
    // cargo that is running this test so the guard is location-independent.
    let output = Command::new(env!("CARGO"))
        .args(["tree", "-p", "iroh-rooms-crypto", "--prefix", "none"])
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            // If cargo is somehow unavailable, fail loudly — silently
            // passing would defeat the guard.
            panic!("failed to invoke `cargo tree` for the banned-dep guard: {e}");
        }
    };
    assert!(
        output.status.success(),
        "`cargo tree` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    // Each line of `--prefix none` output is `name version ...`; match on the
    // leading crate name token to avoid substring false positives (e.g. a
    // crate named `tokio-util` would not match `tokio` here, but a direct
    // `tokio` dependency would).
    for line in tree.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        // Strip a possible "(feature)" or source parenthetical; the name
        // token is what precedes the version.
        let name = first.split(' ').next().unwrap_or(first);
        for banned in BANNED {
            assert_ne!(
                name, *banned,
                "banned dependency `{banned}` appears in iroh-rooms-crypto's tree:\n{tree}"
            );
        }
    }
}
