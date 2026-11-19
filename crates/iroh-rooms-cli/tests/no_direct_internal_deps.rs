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

const INTERNAL_PACKAGES: &[&str] = &["iroh-rooms-core", "iroh-rooms-net"];

/// Package names of every dependency `cargo metadata` reports for `manifest`,
/// including dev-dependencies and renames (`rc = { package = "iroh-rooms-core" }`).
/// `name` is the package, not the manifest key.
fn dependency_package_names(manifest: &Path) -> Vec<String> {
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .unwrap_or_else(|err| panic!("cargo metadata failed to start: {err}"));
    assert!(
        output.status.success(),
        "cargo metadata failed for {}: {}",
        manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must be JSON");
    let manifest_canon = manifest
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonicalize {}: {err}", manifest.display()));
    let package = meta["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .find(|pkg| {
            let path = pkg["manifest_path"].as_str().expect("manifest_path");
            Path::new(path)
                .canonicalize()
                .is_ok_and(|canon| canon == manifest_canon)
        })
        .unwrap_or_else(|| panic!("metadata has no package for {}", manifest.display()));
    package["dependencies"]
        .as_array()
        .expect("dependencies array")
        .iter()
        .map(|dep| dep["name"].as_str().expect("dependency name").to_owned())
        .collect()
}

fn internal_dependencies(manifest: &Path) -> Vec<String> {
    dependency_package_names(manifest)
        .into_iter()
        .filter(|name| INTERNAL_PACKAGES.contains(&name.as_str()))
        .collect()
}

fn write_dodge_manifest(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir.join("src")).expect("temp crate src");
    std::fs::write(dir.join("src/lib.rs"), "").expect("temp lib");
    let manifest = dir.join("Cargo.toml");
    std::fs::write(&manifest, body).expect("temp manifest");
    manifest
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
    let offending = internal_dependencies(&crate_root().join("Cargo.toml"));
    assert!(
        offending.is_empty(),
        "the CLI manifest must not depend on the internal protocol crates directly. \
         Found: {offending:?}"
    );
}

#[test]
fn renamed_core_dependency_is_rejected() {
    let dir = std::env::temp_dir().join(format!(
        "iroh-rooms-dep-dodge-rename-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let core = crate_root().join("../iroh-rooms-core");
    let manifest = write_dodge_manifest(
        &dir,
        &format!(
            "[package]\nname = \"dodge-rename\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nrc = {{ package = \"iroh-rooms-core\", path = \"{}\" }}\n",
            core.display()
        ),
    );
    let found = internal_dependencies(&manifest);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(found, vec!["iroh-rooms-core".to_owned()]);
}

#[test]
fn dev_dependency_table_key_is_rejected() {
    let dir =
        std::env::temp_dir().join(format!("iroh-rooms-dep-dodge-table-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let net = crate_root().join("../iroh-rooms-net");
    let manifest = write_dodge_manifest(
        &dir,
        &format!(
            "[package]\nname = \"dodge-table\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dev-dependencies.iroh-rooms-net]\npath = \"{}\"\n",
            net.display()
        ),
    );
    let found = internal_dependencies(&manifest);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(found, vec!["iroh-rooms-net".to_owned()]);
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
