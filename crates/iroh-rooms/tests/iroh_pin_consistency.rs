//! Manifest-level pin-drift tripwire for the iroh re-export (issue #87, R1 / D6).
//!
//! The façade re-exports iroh's `EndpointAddr` / `EndpointId` / `SecretKey` /
//! `Endpoint` *verbatim* from `experimental::session`. Those re-exports are the same
//! types `iroh-rooms-net`'s public `Node` API names **only while both crates pin the
//! exact same `iroh` version** (spec §3.2): Cargo unifies two identical `=x.y.z` pins
//! into one crate instance, so the re-exported `EndpointAddr` shares net's `TypeId`.
//! If the pins ever diverge, Cargo resolves *two* `iroh` crates and the façade's
//! `EndpointAddr` silently becomes a different type from net's — the exact
//! "two-crates" trap #87 exists to kill (R1).
//!
//! D6/OQ2 deferred hoisting the pin to `[workspace.dependencies]`, so today the two
//! pins are "kept in sync by hand" and cross-referenced in prose comments. This test
//! makes that hand-maintained invariant enforced: a one-sided `iroh` bump fails here
//! (fast, offline, legible) rather than surfacing as a confusing "expected `X`, found
//! `X`" error in a downstream consumer. It complements `experimental_surface.rs`'s
//! `iroh_transport_reexports_are_the_net_api_types` compile-time identity guard by
//! catching the drift one layer earlier — at the manifest declaration.
//!
//! Not feature-gated: it only reads `Cargo.toml` files, so it runs on a
//! default-features build too (no `experimental` needed).

use std::path::{Path, PathBuf};

/// The workspace root — this crate is `crates/iroh-rooms`, two levels down.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist")
}

/// Read a workspace-relative manifest (e.g. `crates/iroh-rooms-net/Cargo.toml`).
fn read_manifest(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{relative} must exist at {}", path.display()))
}

/// Extract the version requirement of the direct dependency on the crate *literally*
/// named `iroh` (not `iroh-rooms*`, whose keys carry a `-` after `iroh`). Handles both
/// the plain-string form `iroh = "=1.0.1"` (net) and the table form
/// `iroh = { version = "=1.0.1", optional = true }` (façade) by taking the line's
/// first double-quoted token as the requirement. Comment lines are skipped.
fn iroh_pin(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let is_iroh_key = trimmed
            .strip_prefix("iroh")
            .is_some_and(|rest| matches!(rest.chars().next(), Some('=' | '.' | ' ' | '\t')));
        if !is_iroh_key {
            continue;
        }
        let after_open = line.split_once('"')?.1;
        let (version, _) = after_open.split_once('"')?;
        return Some(version.to_string());
    }
    None
}

#[test]
fn facade_and_net_pin_iroh_identically() {
    let facade = read_manifest("crates/iroh-rooms/Cargo.toml");
    let net = read_manifest("crates/iroh-rooms-net/Cargo.toml");

    let facade_pin = iroh_pin(&facade)
        .expect("façade Cargo.toml must declare a direct `iroh` dependency (issue #87 §3.2)");
    let net_pin =
        iroh_pin(&net).expect("iroh-rooms-net Cargo.toml must declare a direct `iroh` dependency");

    assert_eq!(
        facade_pin, net_pin,
        "issue #87 R1/D6: the façade's `iroh` pin ({facade_pin}) must stay byte-identical to \
         iroh-rooms-net's ({net_pin}) so Cargo unifies to one `iroh` crate and the re-exported \
         transport identities are the SAME types net's public API names. A one-sided bump \
         reintroduces the two-crate bug — bump both, or hoist the pin to \
         [workspace.dependencies] (D6/OQ2)."
    );
}

#[test]
fn facade_pins_iroh_with_an_exact_requirement() {
    // §3.2: the pin must be exact (`=x.y.z`), never a caret/range. An inexact
    // requirement could let a future `cargo update` pick a different patch than net
    // resolved, splitting the re-exported types even while the strings look "close".
    let facade = read_manifest("crates/iroh-rooms/Cargo.toml");
    let pin = iroh_pin(&facade).expect("façade Cargo.toml must declare a direct `iroh` dependency");
    assert!(
        pin.starts_with('='),
        "issue #87 §3.2: the façade's iroh pin must be an exact `=` requirement (found `{pin}`) \
         so it cannot resolve to a different version than iroh-rooms-net"
    );
}
