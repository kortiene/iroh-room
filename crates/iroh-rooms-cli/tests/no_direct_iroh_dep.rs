//! Regression guard for issue #87 AC2: the reference CLI consumer drives the
//! online session API **entirely through the `iroh_rooms` façade** and carries no
//! direct `iroh` dependency.
//!
//! Issue #87 re-exports iroh's transport identities (`EndpointAddr` / `EndpointId`
//! / `SecretKey`, plus `Endpoint`) from `iroh_rooms::experimental::session` so a
//! downstream consumer need not add — and keep byte-identical — its own direct
//! `iroh` pin. The CLI is the *proving* consumer (spec §4.2): its direct `iroh`
//! dependency was deleted and every `iroh::` path (both `use` statements and inline
//! fully-qualified refs, e.g. the old `iroh::EndpointId::from_bytes` in `pipe.rs`)
//! was routed through the façade.
//!
//! That property is currently true only *by absence* — nothing stops a future edit
//! from re-adding `iroh = "=1.0.1"` to the CLI or writing a fresh `iroh::` path, and
//! either would still compile while silently breaking the "imports only through the
//! façade" claim the issue exists to make true. These tests turn the spec's §4.5
//! `grep -n … iroh::` audit into an enforced invariant.
//!
//! Deterministic and offline: they only read this crate's own `Cargo.toml` and
//! `src/` tree — no binary execution, no network, no external services. (This test
//! file lives under `tests/`, so it is never scanned by the `src/` walk below and
//! cannot trip its own `"iroh::"` string literal.)

use std::path::{Path, PathBuf};

/// This crate's root (`crates/iroh-rooms-cli`).
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// This crate's `Cargo.toml` contents.
fn manifest_toml() -> String {
    let path = crate_root().join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("CLI Cargo.toml must exist at {}", path.display()))
}

/// True when a `Cargo.toml` `line` declares a dependency on the crate *literally*
/// named `iroh` — as opposed to `iroh-rooms` / `iroh-rooms-core` / `iroh-rooms-net`,
/// whose keys all have a `-` immediately after `iroh`. Comment lines are ignored so
/// the prose cross-reference comments (which mention `iroh`) don't false-positive.
fn declares_bare_iroh_key(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    trimmed
        .strip_prefix("iroh")
        .is_some_and(|rest| matches!(rest.chars().next(), Some('=' | '.' | ' ' | '\t')))
}

/// True when a Rust source `line` names the bare `iroh` crate — either a qualified
/// path (`iroh::…`, which `iroh_rooms::` / `iroh_rooms_core::` / `iroh_rooms_net::`
/// never contain because they carry `_` after `iroh`) or a crate-level `use` /
/// `extern crate` of it. This mirrors the spec §4.5 audit grep.
fn names_bare_iroh(line: &str) -> bool {
    if line.contains("iroh::") {
        return true;
    }
    let trimmed = line.trim_start();
    for prefix in ["use iroh", "extern crate iroh"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            if matches!(rest.chars().next(), Some(';' | ' ' | ':' | '\t')) {
                return true;
            }
        }
    }
    false
}

/// Every `.rs` file under `src/` — production code plus the inline `#[cfg(test)]`
/// modules (spec D5 migrated those too, so the whole tree must be `iroh::`-free).
fn cli_source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rs(&crate_root().join("src"), &mut files);
    assert!(
        !files.is_empty(),
        "CLI src/ must contain at least one .rs file to scan"
    );
    files
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|_| panic!("must read dir {}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry must be readable").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn cli_manifest_declares_no_direct_iroh_dependency() {
    // AC2: the direct `iroh` dependency is deleted from crates/iroh-rooms-cli/Cargo.toml
    // (both `[dependencies]` and `[dev-dependencies]`). A re-added bare `iroh` key here
    // resurrects the version-skew trap #87 kills — even though it would still compile.
    let toml = manifest_toml();
    let offending: Vec<&str> = toml
        .lines()
        .filter(|line| declares_bare_iroh_key(line))
        .map(str::trim)
        .collect();
    assert!(
        offending.is_empty(),
        "issue #87 AC2: crates/iroh-rooms-cli/Cargo.toml must declare NO direct `iroh` \
         dependency — the transport identities come through \
         `iroh_rooms::experimental::session`. Found: {offending:?}"
    );
}

#[test]
fn cli_source_has_no_direct_iroh_paths() {
    // AC2: no `iroh::` path (nor a crate-level `use iroh` / `extern crate iroh`) may
    // remain in CLI source; each was rerouted to the façade re-export. This is the
    // spec §4.5 grep, enforced.
    let mut hits: Vec<String> = Vec::new();
    for file in cli_source_files() {
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|_| panic!("must read {}", file.display()));
        for (idx, line) in text.lines().enumerate() {
            if names_bare_iroh(line) {
                hits.push(format!("{}:{}: {}", file.display(), idx + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "issue #87 AC2: no CLI source may name the bare `iroh` crate — route through \
         `iroh_rooms::experimental::session::{{EndpointAddr, EndpointId, SecretKey}}` \
         instead. Found:\n{}",
        hits.join("\n")
    );
}

#[test]
fn cli_reaches_transport_identities_through_the_facade() {
    // Positive complement to the two negative guards above: prove the migration
    // *re-routed* the imports (rather than dropping the feature) — the façade session
    // path and each transport-identity name are still present in the tree, so the CLI
    // genuinely exercises the #87 re-exports.
    let all: String = cli_source_files()
        .iter()
        .map(|file| std::fs::read_to_string(file).unwrap_or_default())
        .collect();
    assert!(
        all.contains("iroh_rooms::experimental::session"),
        "CLI must import its transport identities through the façade session module \
         (issue #87 §4.2)"
    );
    for ty in ["EndpointAddr", "EndpointId", "SecretKey"] {
        assert!(
            all.contains(ty),
            "CLI must still name the `{ty}` transport identity (now via the façade re-export)"
        );
    }
}
