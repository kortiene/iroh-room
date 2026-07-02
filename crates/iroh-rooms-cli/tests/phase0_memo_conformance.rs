//! Structural + source-consistency conformance tests for the Phase 0 go/no-go
//! memo (`PHASE-0-GO-NO-GO.md`, issue #15 / `IR-0011`).
//!
//! These tests are deterministic and offline: no network, no binary execution,
//! no external services. They verify the #15 acceptance criteria purely by
//! reading the memo Markdown, and — where the memo makes a machine-checkable
//! factual claim — cross-check it against the actual repository source so the
//! memo cannot silently drift out of date:
//!
//! * every relative link the memo cites resolves to a real path (AC / Test Plan
//!   "all cited paths exist");
//! * the pinned dependency versions match the crate manifests (§6.5);
//! * the Gate A verdict matches the `spike-nat` results state (§5 / §6.7);
//! * the quoted empty `DEFERRED` taxonomy list matches `taxonomy.rs` (§6.2).

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/iroh-rooms-cli; workspace root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{rel} must exist at {}", path.display()))
}

fn memo() -> String {
    read("PHASE-0-GO-NO-GO.md")
}

// ── Placement & back-links (spec §3, §8) ─────────────────────────────────────

#[test]
fn memo_exists_at_repo_root() {
    // Spec §3 / AC: the memo lives at PHASE-0-GO-NO-GO.md beside PHASE-0-SPIKE.md.
    assert!(
        workspace_root().join("PHASE-0-GO-NO-GO.md").exists(),
        "PHASE-0-GO-NO-GO.md must exist at the repo root"
    );
}

#[test]
fn memo_links_back_to_spike_and_prd() {
    // Spec §4.0 / §8: the header must link back to the plan it closes out and to
    // the PRD roadmap section that scoped Phase 0.
    let content = memo();
    assert!(
        content.contains("PHASE-0-SPIKE.md"),
        "memo must link back to PHASE-0-SPIKE.md (the plan it closes out)"
    );
    assert!(
        content.contains("PRD.v0.3.md"),
        "memo must link to PRD.v0.3.md §19 (Phase 0 roadmap)"
    );
}

// ── Gate results — AC1 (spec §6.2) ───────────────────────────────────────────

#[test]
fn memo_documents_every_gate_a_through_e() {
    // AC1: Gate A–E status is documented — each gate must be named.
    let content = memo();
    for gate in &["Gate A", "Gate B", "Gate C", "Gate D", "Gate E"] {
        assert!(
            content.contains(gate),
            "AC1: memo must document {gate} (spec §6.2)"
        );
    }
    // The three status verdicts used across the table must all appear.
    for status in &["PENDING", "GO", "CONDITIONAL"] {
        assert!(
            content.contains(status),
            "memo gate table must use the {status} verdict (spec §6.2)"
        );
    }
}

#[test]
fn memo_documents_both_soft_gates() {
    // Spec §6.2: the two soft gates (Day 8 Blob ACL, Day 9 Live Pipe) are part of
    // the gate roll-up and must not be dropped.
    let content = memo();
    assert!(
        content.contains("Blob") && content.contains("Pipe"),
        "memo must document the Blob (Day 8) and Pipe (Day 9) soft gates (spec §6.2)"
    );
}

#[test]
fn memo_marks_gate_a_pending_with_honest_framing() {
    // AC1 + spec §10 highest risk: Gate A is the only un-measured gate. The memo
    // must mark it PENDING and state plainly that a green loopback run is NOT Gate
    // A (CI cannot prove NAT traversal) — this is the load-bearing honesty guard.
    let content = memo();
    assert!(
        content.contains("PENDING"),
        "AC1: memo must mark Gate A PENDING (spec §6.2)"
    );
    assert!(
        content.contains("loopback run is NOT Gate A") || content.contains("prove NAT traversal"),
        "memo must state that a green loopback run is NOT Gate A / CI cannot prove NAT traversal \
         (spec §10 — do not overclaim Gate A as green)"
    );
}

// ── Decisions ADR-1 / ADR-2 — AC2 (spec §6.3, §6.4) ──────────────────────────

#[test]
fn memo_confirms_adr1_by_measurement() {
    // AC2: ADR-1 (full-mesh direct QUIC) is confirmed/revised with measured
    // evidence. The memo must name ADR-1, mark it confirmed, and cite the
    // spike-transport measurement.
    let content = memo();
    let lower = content.to_lowercase();
    assert!(
        content.contains("ADR-1") && lower.contains("confirm"),
        "AC2: memo must confirm (or revise) ADR-1 (spec §6.3)"
    );
    assert!(
        content.contains("spike-transport"),
        "AC2: ADR-1 confirmation must cite the spike-transport measured evidence (spec §6.3)"
    );
}

#[test]
fn memo_confirms_adr2_and_discloses_no_docs_benchmark() {
    // AC2: ADR-2 (hand-rolled SQLite signed log + bounded recent-sync) confirmed,
    // AND the honest caveat that no head-to-head iroh-docs benchmark was run
    // (spec §6.4 / §10 risk 2 — do not overclaim a measured comparison).
    let content = memo();
    let lower = content.to_lowercase();
    assert!(
        content.contains("ADR-2") && lower.contains("confirm"),
        "AC2: memo must confirm (or revise) ADR-2 (spec §6.4)"
    );
    assert!(
        content.contains("head-to-head") && content.contains("spike-sync"),
        "AC2: memo must disclose that no head-to-head docs benchmark was run and no spike-sync \
         crate exists (spec §6.4 caveat)"
    );
}

// ── Recommendation — AC4 (spec §6.1, §6.7) ───────────────────────────────────

#[test]
fn memo_states_explicit_go_no_go_recommendation() {
    // AC4: an explicit MVP go/no-go recommendation. With Gate A pending the
    // prescribed call is CONDITIONAL GO (spec §6.7); the sentence must be present.
    let content = memo();
    assert!(
        content.contains("CONDITIONAL GO"),
        "AC4: memo must state an explicit MVP recommendation (CONDITIONAL GO while Gate A pends; \
         spec §6.1/§6.7)"
    );
}

// ── Failed-gate mitigation — AC3 (spec §6.7) ─────────────────────────────────

#[test]
fn memo_gives_gate_a_mitigation_and_exit_condition() {
    // AC3: the one not-green gate (A) must carry a concrete mitigation/descope.
    // The memo must name the P0 blocking exit condition and the relay-fallback
    // escalation branch.
    let content = memo();
    assert!(
        content.contains("Blocking exit condition"),
        "AC3: memo must state the P0 blocking exit condition for Gate A (spec §6.7)"
    );
    assert!(
        content.contains("relay fallback"),
        "AC3: memo must give the relay-fallback mitigation if Gate A returns NO-GO (spec §6.7)"
    );
}

// ── Evidence matrix — Test Plan (spec §6.8 / §7) ─────────────────────────────

#[test]
fn memo_evidence_matrix_covers_all_child_issues() {
    // Test Plan: "review all Phase 0 child issues and link measured outputs." Every
    // dependency issue (#6–#14 and #43) must appear in the evidence matrix.
    let content = memo();
    let mut missing = Vec::new();
    for issue in &[
        "#6", "#7", "#8", "#9", "#10", "#11", "#12", "#13", "#14", "#43",
    ] {
        if !content.contains(issue) {
            missing.push(*issue);
        }
    }
    assert!(
        missing.is_empty(),
        "Test Plan: evidence matrix is missing these Phase 0 child issues: {missing:?} (spec §7)"
    );
}

// ── Dead-link check — AC / Test Plan "all cited paths exist" ──────────────────

// Extract every markdown link target (`](target)`), dropping an optional
// `"title"` suffix and any `#fragment`. Only the link form is parsed (not inline
// `code`), so ALPN strings like `/iroh-rooms/event/1` are not mistaken for paths.
fn markdown_link_targets(md: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut idx = 0;
    while let Some(rel) = md[idx..].find("](") {
        let start = idx + rel + 2;
        if let Some(end_rel) = md[start..].find(')') {
            let raw = &md[start..start + end_rel];
            let target = raw
                .split_whitespace()
                .next()
                .unwrap_or("")
                .split('#')
                .next()
                .unwrap_or("");
            if !target.is_empty() {
                targets.push(target.to_string());
            }
            idx = start + end_rel + 1;
        } else {
            break;
        }
    }
    targets
}

#[test]
fn every_relative_link_in_memo_resolves() {
    // AC / Test Plan: every cited path must resolve to a real file or directory in
    // the repo. A renamed/removed crate file that the memo still links to fails
    // here — the memo's traceability is only as good as its links.
    let content = memo();
    let root = workspace_root();
    let mut checked = 0usize;
    let mut missing = Vec::new();
    for target in markdown_link_targets(&content) {
        if target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with("mailto:")
        {
            continue;
        }
        checked += 1;
        if !root.join(&target).exists() {
            missing.push(target);
        }
    }
    // Guard against a broken parser silently making this test vacuous.
    assert!(
        checked >= 20,
        "expected the memo to cite many repo paths; only {checked} link targets parsed — the link \
         extractor may be broken"
    );
    assert!(
        missing.is_empty(),
        "memo cites paths that do not exist in the repo (dead links break AC / Test Plan): {missing:?}"
    );
}

// ── Source-consistency guards (spec §5 re-verification, encoded as tests) ─────

#[test]
fn memo_gate_a_verdict_matches_spike_nat_source() {
    // Spec §5.1 / §6.7: the memo's Gate A verdict is only honest if it matches the
    // repo. Gate A is "measured" once per-run JSON is committed under
    // crates/spike-nat/results/ (and the placeholder results table is replaced).
    // If it has been measured, the memo must NOT still say PENDING; if not, the
    // memo must say PENDING and recommend CONDITIONAL.
    let content = memo();
    let memo_marks_gate_a_pending = content.contains("PENDING");

    let results_md = read("crates/spike-nat/results/results.md");
    let placeholder_present = results_md.contains("pending manual two-host run");

    let results_dir = workspace_root().join("crates/spike-nat/results");
    let has_run_json = std::fs::read_dir(&results_dir)
        .expect("crates/spike-nat/results must exist")
        .filter_map(Result::ok)
        .any(|e| e.path().extension().is_some_and(|ext| ext == "json"));

    let gate_a_measured = has_run_json || !placeholder_present;

    if gate_a_measured {
        assert!(
            !memo_marks_gate_a_pending,
            "spike-nat now has measured Gate A results (per-run JSON present or the placeholder \
             results table is gone) but the memo still marks Gate A PENDING — update §6.2/§6.7 and \
             switch the recommendation to GO per spec §6.7"
        );
    } else {
        assert!(
            memo_marks_gate_a_pending,
            "spike-nat Gate A is still unmeasured (no per-run JSON, placeholder results table) — \
             the memo must mark Gate A PENDING (spec §6.2)"
        );
        assert!(
            content.contains("CONDITIONAL"),
            "with Gate A unmeasured the memo's recommendation must be CONDITIONAL (spec §6.7)"
        );
    }
}

// Extract the version pin the memo records for `dep` from its §6.5 table row,
// e.g. from `| `iroh` | `=1.0.1` | … |` returns `=1.0.1`.
fn memo_pin(memo: &str, dep: &str) -> Option<String> {
    let needle = format!("| `{dep}` |");
    let line = memo.lines().find(|l| l.contains(&needle))?;
    let pos = line.find(&needle)? + needle.len();
    let rest = line[pos..].trim_start().strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

// Extract the version string a crate manifest pins for a simple `dep = "…"` entry.
fn manifest_pin(manifest: &str, dep: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((lhs, rhs)) = line.split_once('=') {
            if lhs.trim() == dep {
                let rhs = rhs.trim().trim_start_matches('"');
                let end = rhs.find('"')?;
                return Some(rhs[..end].to_string());
            }
        }
    }
    None
}

#[test]
fn memo_pinned_versions_match_crate_manifests() {
    // Spec §6.5 / §10 "stale version numbers": the memo's pin table is the confirmed
    // versions actually used. A dependency bump that isn't mirrored here makes the
    // memo ship a wrong pin — catch that drift. (iroh-base is not a direct dep, so
    // it is not cross-checkable and is intentionally excluded.)
    let content = memo();
    let checks = [
        ("iroh", "crates/iroh-rooms-net/Cargo.toml"),
        ("iroh-gossip", "crates/spike-transport/Cargo.toml"),
        ("iroh-blobs", "crates/iroh-rooms-net/Cargo.toml"),
        ("ed25519-dalek", "crates/iroh-rooms-core/Cargo.toml"),
    ];
    for (dep, manifest_path) in checks {
        let manifest = read(manifest_path);
        let actual = manifest_pin(&manifest, dep)
            .unwrap_or_else(|| panic!("{manifest_path} must pin {dep} as a version string"));
        let claimed = memo_pin(&content, dep)
            .unwrap_or_else(|| panic!("memo §6.5 pin table must have a row for `{dep}`"));
        assert_eq!(
            claimed, actual,
            "memo §6.5 records `{dep} = {claimed}` but {manifest_path} pins `{actual}` — update the \
             memo's pinned-dependency table (spec §6.5)"
        );
    }
}

#[test]
fn memo_empty_deferred_claim_matches_taxonomy_source() {
    // Spec §6.2: the memo cites the taxonomy-completeness gate's empty DEFERRED list
    // as Gate B evidence, quoting the exact declaration. If a taxonomy outcome is
    // ever deferred (DEFERRED gains an entry), that Gate B claim goes stale — this
    // ties the quoted claim to the real source so it can't rot.
    let content = memo();
    assert!(
        content.contains("const DEFERRED: &[(&str, &str)] = &[]"),
        "memo must quote the empty DEFERRED taxonomy declaration as Gate B evidence (spec §6.2)"
    );
    let taxonomy = read("crates/iroh-rooms-core/tests/conformance/taxonomy.rs");
    assert!(
        taxonomy.contains("const DEFERRED: &[(&str, &str)] = &[];"),
        "taxonomy.rs no longer declares an empty DEFERRED list — the memo's Gate B 'empty DEFERRED' \
         claim is now stale (spec §6.2)"
    );
}

// ── Required structure & ordering (spec §4) ──────────────────────────────────

#[test]
fn memo_contains_all_required_sections_in_order() {
    // Spec §4: the memo MUST contain these sections, in this order. Anchors match the
    // section titles (not their numbers), so renumbering does not break the test but a
    // dropped or reordered section does. Each position comes from `find` (first match),
    // which is a char boundary — no unsafe byte slicing.
    let content = memo();
    let ordered_anchors = [
        "TL;DR",
        "Gate results",
        "Transport decision",
        "Sync substrate decision",
        "Pinned dependency observations",
        "Residual risks accepted for MVP",
        "Failed / not-green gate",
        "Evidence / traceability matrix",
    ];
    let mut last_pos = 0usize;
    let mut last_anchor = "<start>";
    for anchor in ordered_anchors {
        let pos = content.find(anchor).unwrap_or_else(|| {
            panic!("spec §4: memo is missing the required section heading containing {anchor:?}")
        });
        assert!(
            pos >= last_pos,
            "spec §4: section {anchor:?} appears before {last_anchor:?}; required order is \
             {ordered_anchors:?}"
        );
        last_pos = pos;
        last_anchor = anchor;
    }
}

// ── Residual risks — spec §6.6 ───────────────────────────────────────────────

#[test]
fn memo_residual_risks_flag_admin_key_as_single_largest() {
    // Spec §6.6: the residual-risk roll-up must call out the admin-key
    // compromise/loss risk as the SINGLE LARGEST residual (the spec's "call this out
    // prominently"), and mark residuals accepted/descoped for MVP. Line-based so the
    // multi-byte em-dashes in the memo never cause a byte-boundary panic.
    let content = memo();
    let flagged = content
        .lines()
        .find(|l| l.contains("SINGLE LARGEST"))
        .expect("spec §6.6: memo must flag a residual as the SINGLE LARGEST");
    let flagged_lower = flagged.to_lowercase();
    assert!(
        flagged_lower.contains("admin key") || flagged_lower.contains("admin-key"),
        "spec §6.6: the SINGLE LARGEST residual must be the admin-key compromise/loss risk; \
         got line: {flagged:?}"
    );
    assert!(
        content.contains("Accepted"),
        "spec §6.6: residual risks must be marked Accepted (or Descope) for MVP"
    );
}

#[test]
fn memo_single_largest_residual_matches_spike_source() {
    // Spec §6.6: the memo rolls up PHASE-0-SPIKE.md's "Residual Risks & Open
    // Decisions" list. That source document itself designates the admin-key
    // risk as "the single largest operational/security residual" — tie the
    // memo's SINGLE LARGEST claim to that source sentence so a future edit to
    // the spike doc's risk ranking (e.g. a new risk overtakes admin-key) can't
    // silently leave the memo's roll-up stale.
    let spike = read("PHASE-0-SPIKE.md");
    let source_line = spike
        .lines()
        .find(|l| {
            l.to_lowercase()
                .contains("single largest operational/security residual")
        })
        .expect(
            "PHASE-0-SPIKE.md must still designate a single largest operational/security \
             residual for the memo to roll up (spec §6.6)",
        );
    assert!(
        source_line.to_lowercase().contains("admin key")
            || source_line.to_lowercase().contains("admin-key"),
        "PHASE-0-SPIKE.md's single-largest residual is no longer the admin key/admin-key risk \
         (got: {source_line:?}) — the memo's §6.6 SINGLE LARGEST claim is now stale"
    );
}

// ── Evidence matrix — IR-number coverage (spec §7 / Test Plan) ────────────────

#[test]
fn memo_evidence_matrix_maps_every_ir_number() {
    // Test Plan / spec §7: the matrix maps each Phase 0 child issue to its IR number.
    // The existing coverage test checks `#issue` numbers; this checks the IR side of
    // the mapping the memo itself notes can be off-by-reference (IR-0007/IR-0009 land
    // under the IR number, not the `#`). IR-0011 is this memo, not a dependency.
    let content = memo();
    let mut missing = Vec::new();
    for ir in &[
        "IR-0002", "IR-0003", "IR-0004", "IR-0005", "IR-0006", "IR-0007", "IR-0008", "IR-0009",
        "IR-0010", "IR-0012",
    ] {
        if !content.contains(ir) {
            missing.push(*ir);
        }
    }
    assert!(
        missing.is_empty(),
        "spec §7: evidence matrix is missing IR numbers: {missing:?}"
    );
}

// ── Recommendation placement — AC4 (spec §6.1 + §6.7) ────────────────────────

#[test]
fn memo_recommendation_appears_in_tldr_and_mitigation() {
    // AC4 / spec §6.7: the recommendation must be an explicit sentence at the TOP of
    // the memo (§6.1 TL;DR) AND restated in the failed-gate / go-no-go section (§6.7).
    // A single mention buried mid-document does not satisfy "explicit up front and
    // restated with conditions." Slice bounds come from `find` results (char
    // boundaries), so the slicing is safe.
    let content = memo();
    let gate_results = content
        .find("Gate results")
        .expect("memo must have a Gate results section");
    let mitigation = content
        .find("Failed / not-green gate")
        .expect("memo must have a failed-gate / go-no-go section");

    let tldr = &content[..gate_results];
    let tail = &content[mitigation..];
    assert!(
        tldr.contains("CONDITIONAL GO"),
        "AC4: the explicit MVP recommendation (CONDITIONAL GO) must appear up front in the TL;DR \
         (spec §6.1), before the Gate results section"
    );
    assert!(
        tail.contains("CONDITIONAL GO"),
        "AC4: the recommendation must be restated in the failed-gate / go-no-go section (spec §6.7)"
    );
}

// ── Pin table completeness — spec §6.5 ───────────────────────────────────────

#[test]
fn memo_pin_table_lists_all_confirmed_crates() {
    // Spec §6.5: the pinned-dependency table records every confirmed crate, including
    // the ones NOT cross-checkable against a direct manifest dep (iroh-base, ciborium,
    // blake3) and so not covered by `memo_pinned_versions_match_crate_manifests`. A
    // dropped row silently narrows the churn-budget picture the section is there to give.
    let content = memo();
    for dep in &[
        "iroh",
        "iroh-base",
        "iroh-gossip",
        "iroh-blobs",
        "ed25519-dalek",
        "ciborium",
        "blake3",
    ] {
        assert!(
            memo_pin(&content, dep).is_some(),
            "spec §6.5: memo pin table must have a row for `{dep}`"
        );
    }
}
