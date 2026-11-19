//! Docs, feature-gate, and public-SDK reachability pins.
//!
//! Refs #343. The fences in `docs/reachability-inventory.md` are the stable and
//! experimental façade exports. Phrase checks stop user-facing docs from
//! advertising encryption or member-removal gaps that the shipped CLI does not
//! have.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_repo(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read_fence(doc: &str, name: &str) -> BTreeSet<String> {
    let needle = format!("```text\n{name}\n");
    let start = doc
        .find(&needle)
        .unwrap_or_else(|| panic!("docs/reachability-inventory.md is missing the {name} fence"));
    let body = &doc[start + needle.len()..];
    let end = body
        .find("```")
        .unwrap_or_else(|| panic!("docs/reachability-inventory.md {name} fence is unclosed"));
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect()
}

fn strip_comments(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        if let Some(idx) = line.find("//") {
            out.push_str(&line[..idx]);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn take_ident(source: &str) -> String {
    source
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

fn match_brace(source: &str, open: usize) -> usize {
    let mut depth = 0i32;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + offset;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced brace in façade source");
}

fn peel_inline_modules(source: &str) -> (String, Vec<(String, String)>) {
    let mut found = Vec::new();
    let mut rest = String::new();
    let mut i = 0;
    while i < source.len() {
        if source[i..].starts_with("pub mod ") {
            let after = i + "pub mod ".len();
            let name = take_ident(&source[after..]);
            if !name.is_empty() {
                let mut j = after + name.len();
                let bytes = source.as_bytes();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'{') {
                    let end = match_brace(source, j);
                    found.push((name, source[j + 1..end].to_string()));
                    i = end + 1;
                    continue;
                }
            }
        }
        let ch = source[i..].chars().next().expect("index on char boundary");
        rest.push(ch);
        i += ch.len_utf8();
    }
    (rest, found)
}

fn semicolon_mods(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(index) = rest.find("pub mod ") {
        let after = &rest[index + "pub mod ".len()..];
        let name = take_ident(after);
        let after_name = &after[name.len()..];
        if !name.is_empty() && after_name.trim_start().starts_with(';') {
            out.push(name);
        }
        rest = &rest[index + "pub mod ".len()..];
    }
    out
}

fn pub_use_stmts(source: &str) -> Vec<String> {
    let mut stmts = Vec::new();
    let mut acc = String::new();
    let mut capturing = false;
    for line in source.lines() {
        if capturing {
            acc.push(' ');
            acc.push_str(line.trim());
            if line.contains(';') {
                stmts.push(std::mem::take(&mut acc));
                capturing = false;
            }
            continue;
        }
        if let Some(index) = line.find("pub use ") {
            capturing = true;
            acc = line[index..].to_string();
            if acc.contains(';') {
                stmts.push(std::mem::take(&mut acc));
                capturing = false;
            }
        }
    }
    stmts
}

fn pub_use_names(stmt: &str) -> Vec<String> {
    let Some(rest) = stmt.split("pub use ").nth(1) else {
        return Vec::new();
    };
    let rest = rest.trim().trim_end_matches(';').trim();
    if let Some(start) = rest.find('{') {
        let end = rest.rfind('}').expect("pub use brace group");
        let prefix = rest[..start].trim().trim_end_matches("::").trim();
        rest[start + 1..end]
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let name = part
                    .split_once(" as ")
                    .map_or(part, |(_, alias)| alias)
                    .trim();
                if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}::{name}")
                }
            })
            .collect()
    } else if rest.is_empty() {
        Vec::new()
    } else {
        vec![rest.to_string()]
    }
}

fn collect_facade(source: &str, module: &str, out: &mut BTreeSet<String>) {
    let stripped = strip_comments(source);
    let (rest, inlines) = peel_inline_modules(&stripped);
    for (name, body) in inlines {
        let path = format!("{module}::{name}");
        out.insert(format!("module {path}"));
        collect_facade(&body, &path, out);
    }
    for name in semicolon_mods(&rest) {
        out.insert(format!("module {module}::{name}"));
    }
    for stmt in pub_use_stmts(&rest) {
        for item in pub_use_names(&stmt) {
            let name = item.rsplit("::").next().unwrap_or(item.as_str());
            out.insert(format!("{module}::{name}"));
        }
    }
}

fn facade_exports() -> (BTreeSet<String>, BTreeSet<String>) {
    let root = workspace_root().join("crates/iroh-rooms/src");
    let mut stable = BTreeSet::new();
    let mut experimental = BTreeSet::new();
    let stable_files = [
        ("lib.rs", "iroh_rooms"),
        ("events.rs", "iroh_rooms::events"),
        ("files.rs", "iroh_rooms::files"),
        ("identity.rs", "iroh_rooms::identity"),
        ("pipes.rs", "iroh_rooms::pipes"),
        ("room.rs", "iroh_rooms::room"),
    ];
    for (file, module) in stable_files {
        let source = fs::read_to_string(root.join(file)).expect(file);
        collect_facade(&source, module, &mut stable);
    }
    let experimental_files = [
        ("experimental/mod.rs", "iroh_rooms::experimental"),
        ("experimental/blob.rs", "iroh_rooms::experimental::blob"),
        (
            "experimental/pipe_runtime.rs",
            "iroh_rooms::experimental::pipe_runtime",
        ),
        (
            "experimental/rotation.rs",
            "iroh_rooms::experimental::rotation",
        ),
        (
            "experimental/session.rs",
            "iroh_rooms::experimental::session",
        ),
        ("experimental/store.rs", "iroh_rooms::experimental::store"),
        ("experimental/sync.rs", "iroh_rooms::experimental::sync"),
    ];
    for (file, module) in experimental_files {
        let source = fs::read_to_string(root.join(file)).expect(file);
        collect_facade(&source, module, &mut experimental);
    }
    // `lib.rs` declares `experimental` beside the stable modules. The stable
    // fence lists that module; the experimental fence lists its contents.
    stable.remove("module iroh_rooms::experimental");
    experimental.insert("module iroh_rooms::experimental".to_string());
    (stable, experimental)
}

fn assert_fence_eq(name: &str, actual: &BTreeSet<String>) {
    let expected = read_fence(&read_repo("docs/reachability-inventory.md"), name);
    let missing: Vec<_> = actual.difference(&expected).cloned().collect();
    let extra: Vec<_> = expected.difference(actual).cloned().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{name} drifted\n\nmissing from the inventory:\n{}\n\n\
         in the inventory, absent from the façade:\n{}\n",
        missing.join("\n"),
        extra.join("\n"),
    );
}

#[test]
fn parser_splits_grouped_reexports_and_inline_modules() {
    let source = "pub mod events;\npub mod prelude {\n    pub use crate::room::{RoomId, Role as MembershipRole};\n}\n";
    let mut got = BTreeSet::new();
    collect_facade(source, "iroh_rooms", &mut got);
    assert!(got.contains("module iroh_rooms::events"));
    assert!(got.contains("module iroh_rooms::prelude"));
    assert!(got.contains("iroh_rooms::prelude::RoomId"));
    assert!(got.contains("iroh_rooms::prelude::MembershipRole"));
    assert!(!got.contains("iroh_rooms::prelude::Role"));
}

#[test]
fn stable_and_experimental_fences_match_facade_sources() {
    let (stable, experimental) = facade_exports();
    assert!(stable.contains("iroh_rooms::room::build_member_removed"));
    assert!(stable.contains("iroh_rooms::room::build_member_left"));
    assert!(
        !stable
            .iter()
            .any(|row| row.contains("build_member_removed_with_rotation")),
        "rotation builder must stay off the stable façade: {stable:?}"
    );
    assert!(experimental
        .contains("iroh_rooms::experimental::rotation::build_member_removed_with_rotation"));
    assert_fence_eq("reachability-sdk-stable", &stable);
    assert_fence_eq("reachability-sdk-experimental", &experimental);
}

#[test]
fn shipped_manifests_do_not_enable_gossip_or_the_40_member_cap() {
    let cli = uncommented(&read_repo("crates/iroh-rooms-cli/Cargo.toml"));
    let sdk = uncommented(&read_repo("crates/iroh-rooms/Cargo.toml"));
    let net = uncommented(&read_repo("crates/iroh-rooms-net/Cargo.toml"));
    let core = uncommented(&read_repo("crates/iroh-rooms-core/Cargo.toml"));
    for (name, text) in [("cli", &cli), ("sdk", &sdk)] {
        assert!(
            !text.contains("gossip_overlay") && !text.contains("large_rooms"),
            "{name} manifest enables a non-shipped topology or cap:\n{text}"
        );
    }
    assert!(
        net.contains("default = []"),
        "iroh-rooms-net default features must stay empty so gossip_overlay is opt-in"
    );
    assert!(
        net.contains("gossip_overlay = ["),
        "the overlay feature must remain available as an explicit opt-in"
    );
    assert!(
        !core.contains("default ="),
        "iroh-rooms-core must not default-enable large_rooms"
    );
    assert!(core.contains("large_rooms = []"));
}

fn uncommented(toml: &str) -> String {
    toml.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn production_sources_do_not_enable_encrypted_content_writes() {
    let crates = workspace_root().join("crates");
    let mut hits = Vec::new();
    let mut files = 0usize;
    for entry in
        fs::read_dir(&crates).unwrap_or_else(|err| panic!("read {}: {err}", crates.display()))
    {
        let src = entry.expect("crate dir").path().join("src");
        if src.is_dir() {
            scan_tree(&src, &mut hits, &mut files);
        }
    }
    assert!(
        files > 0,
        "the production-source scan did not see any crate src"
    );
    assert!(
        hits.is_empty(),
        "production source enables encrypted content writes: {hits:?}"
    );
}

fn scan_tree(dir: &Path, hits: &mut Vec<String>, files: &mut usize) {
    for entry in fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            scan_tree(&path, hits, files);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        *files += 1;
        let text = fs::read_to_string(&path).expect("source");
        if text.contains("encrypted_content_writes: true")
            || text.contains("encrypted_content_writes = true")
        {
            hits.push(path.display().to_string());
        }
    }
}

#[test]
fn user_facing_docs_do_not_advertise_unreachable_capabilities() {
    let template = flat(&read_repo(
        "docs/operations/production-beta-release-notes-template.md",
    ));
    assert!(
        template.contains("Content encryption is not a shipped CLI capability"),
        "release-notes template must say encryption is not a CLI capability"
    );
    assert!(!template.contains("operator rotates"));
    assert!(!template.contains("content-key rotation enabled"));
    assert!(
        !template.contains("ciphertext and the keys"),
        "template must not say a rooms.db copy holds ciphertext plus keys for CLI rooms"
    );

    let threat = flat(&read_repo("docs/security/threat-model.md"));
    assert!(
        !threat.contains("has no member-removal"),
        "threat model still says room remove is unreachable"
    );
    assert!(!threat.contains("authors no removal"));
    assert!(
        !threat.contains("yields only ciphertext"),
        "threat model still claims room content at rest is ciphertext"
    );
    assert!(!threat.contains("operator explicitly rotates"));
    assert!(threat.contains("room remove"));
    assert!(threat.contains("room leave"));

    let protocol = flat(&read_repo("docs/protocol.md"));
    assert!(!protocol.contains("deployment that opts in"));
    assert!(protocol.contains("no shipped CLI, net, or stable SDK call site"));

    let prd = read_repo("PRD.v0.3.md");
    assert!(
        !prd.contains("--tcp localhost"),
        "PRD pipe examples must use an IP clap can parse, not localhost"
    );
    assert!(!prd.contains("room invite <room-id> --expires 24h\n"));
    assert!(prd.contains("room invite <room-id> --invitee <identity-id> --expires 24h"));

    let coverage = read_repo("docs/sdk-coverage.md");
    assert!(
        !coverage.contains("there is no CLI-internal residue"),
        "sdk-coverage must not claim every CLI import has a façade path"
    );
    assert!(coverage.contains("ACTIVE_MEMBER_WARNING_THRESHOLD"));

    let inventory = read_repo("docs/reachability-inventory.md");
    assert!(inventory.contains("Encryption-claims"));
    assert!(inventory.contains("Refs #343"));
    let lowered = inventory.to_lowercase();
    for banned in ["fixes #", "closes #", "resolves #", "close #343"] {
        assert!(
            !lowered.contains(banned),
            "inventory must reference the issue without closing language ({banned})"
        );
    }
}
