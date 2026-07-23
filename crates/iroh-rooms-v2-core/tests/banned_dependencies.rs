//! Banned-dependency guard (spec §8 step 1.7 / §9 CI commands).
//!
//! Asserts that `iroh-rooms-v2-core`'s dependency tree contains NONE of the
//! runtime/store/network crates the pure-core acceptance criteria forbid:
//! `iroh`, `iroh-blobs`, `iroh-gossip`, `tokio`, `rusqlite`. The crate must be a
//! pure crypto + canonical-CBOR + Merkle foundation (spec §1 / §10).
//!
//! This runs `cargo tree -p iroh-rooms-v2-core` at test time (cargo is available
//! in CI; the spec lists this exact command as a verification step).

use std::process::Command;

/// Crate names the pure core must NEVER depend on, directly or transitively.
const BANNED: &[&str] = &["iroh", "iroh-blobs", "iroh-gossip", "tokio", "rusqlite"];

#[test]
fn dependency_tree_contains_no_banned_crates() {
    // `cargo tree` must run against the package's own workspace. Use the current
    // manifest dir so the test is location-independent.
    let output = Command::new(env!("CARGO"))
        .args(["tree", "-p", "iroh-rooms-v2-core", "--prefix", "none"])
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            // If cargo is somehow unavailable, fail loudly — the spec requires
            // this guard to run in CI; silently passing would defeat it.
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
    // leading crate name token to avoid substring false positives (e.g. a crate
    // named `tokio-util` would not match `tokio` here, but a direct `tokio`
    // dependency would).
    for line in tree.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        // Strip a possible "(feature)" or source parenthetical; the name token is
        // what precedes the version.
        let name = first.split(' ').next().unwrap_or(first);
        for banned in BANNED {
            assert_ne!(
                name, *banned,
                "banned dependency `{banned}` appears in iroh-rooms-v2-core's tree:\n{tree}"
            );
        }
    }
}
