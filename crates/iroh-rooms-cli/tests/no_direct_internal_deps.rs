//! Regression guard for RB-SDK-001: the reference CLI consumer names protocol
//! and network types only through the `iroh_rooms` façade.
//!
//! `iroh-rooms-core` and `iroh-rooms-net` stay implementation crates. A direct
//! `use` or path in the CLI would compile, and would silently put the SDK
//! coverage claim back to the partial migration this guard exists to keep
//! closed. `tests/no_direct_iroh_dep.rs` is the sibling guard for the bare
//! `iroh` crate; it mentions the internal crate paths in its own comment so
//! the scanner can tell `iroh::` from `iroh_rooms::`, and is excluded here.

use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest_toml() -> String {
    let path = crate_root().join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("CLI Cargo.toml must exist at {}", path.display()))
}

fn declares_internal_key(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    trimmed.starts_with("iroh-rooms-core") || trimmed.starts_with("iroh-rooms-net")
}

fn names_internal_crate(line: &str) -> bool {
    line.contains("iroh_rooms_core") || line.contains("iroh_rooms_net")
}

fn rust_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rs(dir, &mut files);
    files
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|_| panic!("must read dir {}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry must be readable").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn cli_manifest_has_no_direct_internal_dependency() {
    let toml = manifest_toml();
    let offending: Vec<&str> = toml
        .lines()
        .filter(|line| declares_internal_key(line))
        .map(str::trim)
        .collect();
    assert!(
        offending.is_empty(),
        "the CLI manifest must not depend on the internal protocol crates directly. \
         Found: {offending:?}"
    );
}

#[test]
fn cli_source_and_tests_have_no_direct_internal_paths() {
    let mut files = rust_files_under(&crate_root().join("src"));
    files.extend(rust_files_under(&crate_root().join("tests")));
    files.retain(|path| {
        path.file_name().and_then(|name| name.to_str()) != Some("no_direct_iroh_dep.rs")
            && path.file_name().and_then(|name| name.to_str()) != Some("no_direct_internal_deps.rs")
    });
    assert!(
        !files.is_empty(),
        "CLI src/ and tests/ must contain Rust sources"
    );

    let mut hits: Vec<String> = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("source must be readable");
        for (idx, line) in text.lines().enumerate() {
            if names_internal_crate(line) {
                hits.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "CLI production code and integration tests must import the façade only. \
         Found: {hits:?}"
    );
}
