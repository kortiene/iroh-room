//! `docs/protocol.md` reconciliation gate (IR-0302 / #37).
//!
//! IR-0302 shipped `docs/protocol.md` as the implementer reference. In prose it
//! restates a large set of byte-level values that MUST stay identical to the
//! landed code and the golden conformance fixtures: the reason/flag taxonomy, the
//! `constants.rs` structural bounds and domain-separation context strings, the
//! Tier-1 golden ids, and the vector-to-test map. A Markdown file is not
//! compiled, so nothing otherwise stops it drifting from the code it claims to
//! mirror — exactly the drift risk / follow-up the spec flags.
//!
//! This module closes that gap. It embeds the doc at compile time via
//! `include_str!` (so a moved or deleted doc fails the build) and asserts,
//! against the *same* sources the rest of the suite already trusts, that:
//!
//! * every `RejectReason` / `Flag` `.code()` string (plus the `duplicate`
//!   outcome) appears verbatim, and the core-taxonomy tables introduce no
//!   invented code;
//! * every domain-separation context string and structural bound is stated with
//!   its exact `constants.rs` value;
//! * the Tier-1 golden `room_id` / `event_id` / CSB values match the pinned
//!   fixtures (the CSB is recomputed live, not copied);
//! * every vector-to-test row names a `fn` that actually exists, and the two
//!   `cargo test --test` commands the doc advertises resolve to real files.
//!
//! It is deliberately tolerant of prose — it checks *presence* of load-bearing
//! tokens, not wording — so a rewrite of the surrounding text never fails it;
//! only a factual drift does.

use std::collections::BTreeSet;
use std::path::Path;

use iroh_rooms_core::event::cbor::{decode_canonical, CborValue};
use iroh_rooms_core::event::constants::{
    BIND_CONTEXT, CLOCK_SKEW_FUTURE_MS, DIGEST_LEN, EVENT_CONTEXT, INVITE_CONTEXT,
    MAX_ARTIFACT_REFS, MAX_FILE_NAME_BYTES, MAX_FILE_PROVIDERS, MAX_MESSAGE_BODY_BYTES,
    MAX_MIME_TYPE_BYTES, MAX_PREV_EVENTS, MAX_SHARED_FILE_BYTES, MAX_STATUS_LABEL_BYTES,
    MAX_STATUS_MESSAGE_BYTES, PUBLIC_KEY_LEN, ROOMID_CONTEXT, SHORT_ID_LEN, SIGNATURE_LEN,
};
use iroh_rooms_core::event::content::EventType;
use iroh_rooms_core::event::reject::{Flag, RejectReason};

use super::fixtures;

/// The shipped `docs/protocol.md`, embedded at compile time. The path is relative
/// to this source file (`crates/iroh-rooms-core/tests/conformance/`), so a moved
/// or deleted doc breaks the build — the first line of the "docs are linked" gate.
const DOC: &str = include_str!("../../../../docs/protocol.md");

/// The four vector modules whose `fn vector_NN_*` definitions the doc's vector map
/// links; embedded so the doc-to-source cross-check needs no filesystem access.
const VECTOR_SOURCES: &[&str] = &[
    include_str!("serialization.rs"),
    include_str!("idempotency_ordering.rs"),
    include_str!("membership.rs"),
    include_str!("advisory.rs"),
];

/// Every `RejectReason` (15), hand-mirrored because the enum is
/// `#[non_exhaustive]` and an external test crate cannot reflect it. The count is
/// pinned below, so a new variant forces an update here — which then trips the
/// doc-coverage assert unless the doc is updated too. Mirrors `taxonomy.rs`.
const ALL_REASONS: &[RejectReason] = &[
    RejectReason::UnknownSchemaVersion,
    RejectReason::UnknownEventType,
    RejectReason::NonCanonicalEncoding,
    RejectReason::IdMismatch,
    RejectReason::BadSignature,
    RejectReason::RoomIdMismatch,
    RejectReason::InvalidContent,
    RejectReason::TooManyParents,
    RejectReason::NotGenesisDescended,
    RejectReason::UnboundDevice,
    RejectReason::NotAMember,
    RejectReason::InsufficientRole,
    RejectReason::ExpiredInvite,
    RejectReason::BadCapability,
    RejectReason::RoomFull,
];

/// Every `Flag` (3).
const ALL_FLAGS: &[Flag] = &[Flag::ClockSkew, Flag::Equivocation, Flag::FromRemovedMember];

/// The `duplicate` ignored-outcome is neither a `RejectReason` nor a `Flag`.
const DUPLICATE_CODE: &str = "duplicate";

/// Every `EventType` (10) in the closed MVP registry, hand-mirrored (the enum is
/// a plain closed enum, but an external test crate still cannot iterate it). The
/// count is pinned below, so adding an 11th type forces an update here — which
/// then trips the §6-registry doc-coverage assert unless the doc is updated too.
/// Mirrors `event::content::EventType`.
const ALL_EVENT_TYPES: &[EventType] = &[
    EventType::RoomCreated,
    EventType::MemberInvited,
    EventType::MemberJoined,
    EventType::MemberLeft,
    EventType::MemberRemoved,
    EventType::MessageText,
    EventType::FileShared,
    EventType::PipeOpened,
    EventType::PipeClosed,
    EventType::AgentStatus,
];

/// Every taxonomy code the doc's reason/flag section must account for.
fn known_codes() -> Vec<&'static str> {
    ALL_REASONS
        .iter()
        .map(RejectReason::code)
        .chain(ALL_FLAGS.iter().map(Flag::code))
        .chain(std::iter::once(DUPLICATE_CODE))
        .collect()
}

// ---------------------------------------------------------------------------
// Reason / flag taxonomy: forward coverage + inverse "no invented code".
// ---------------------------------------------------------------------------

#[test]
fn every_reason_and_flag_code_is_documented() {
    // Guard the hand-mirrored lists: if these trip, a taxonomy variant changed and
    // both this module and the doc's reason-code tables need review.
    assert_eq!(
        ALL_REASONS.len(),
        15,
        "the reason taxonomy has exactly 15 variants"
    );
    assert_eq!(
        ALL_FLAGS.len(),
        3,
        "the flag taxonomy has exactly 3 variants"
    );

    for code in known_codes() {
        assert!(
            DOC.contains(code),
            "docs/protocol.md never mentions taxonomy code `{code}` (reason-code drift)"
        );
    }

    // The doc states the rejection count inline ("### Rejections (15 — ...)"); pin
    // it to the actual number of variants so a new reason cannot land with a stale
    // count in the doc.
    assert!(
        DOC.contains(&format!("Rejections ({}", ALL_REASONS.len())),
        "docs/protocol.md must state the rejection count as {}",
        ALL_REASONS.len()
    );
}

#[test]
fn core_taxonomy_tables_name_no_unknown_code() {
    let known = known_codes();
    let tables = core_taxonomy_tables(DOC);
    let cells = leading_code_cells(&tables);

    for token in &cells {
        assert!(
            known.contains(&token.as_str()),
            "docs/protocol.md core-taxonomy table lists `{token}`, which is not a \
             RejectReason/Flag `.code()` — do not invent variants"
        );
    }

    // Sanity: the three core tables (14 + 1 + 3) must have been scanned. Guards a
    // slice/parse regression that would make the inverse check vacuously pass.
    assert!(
        cells.len() >= known.len(),
        "expected to scan >= {} code cells in the core taxonomy tables, saw {}",
        known.len(),
        cells.len()
    );
}

// ---------------------------------------------------------------------------
// Context strings + structural bounds must match `constants.rs`.
// ---------------------------------------------------------------------------

#[test]
fn context_strings_and_structural_bounds_match_constants() {
    // Domain-separation context strings: the exact ASCII bytes an interoperable
    // peer must prepend. A wrong byte here yields a non-interoperable signer.
    for ctx in [EVENT_CONTEXT, ROOMID_CONTEXT, BIND_CONTEXT, INVITE_CONTEXT] {
        let s = std::str::from_utf8(ctx).expect("context strings are ASCII");
        assert!(
            DOC.contains(s),
            "docs/protocol.md must cite context string `{s}` verbatim"
        );
    }

    // Each structural bound: the doc must name the constant AND render its value.
    // Rendering-tolerant (plain / comma-grouped / underscore-grouped all accept),
    // so reformatting never fails the check — only a wrong number does.
    let bounds: &[(&str, u64)] = &[
        ("MAX_PREV_EVENTS", MAX_PREV_EVENTS as u64),
        ("CLOCK_SKEW_FUTURE_MS", CLOCK_SKEW_FUTURE_MS),
        ("MAX_MESSAGE_BODY_BYTES", MAX_MESSAGE_BODY_BYTES as u64),
        ("MAX_SHARED_FILE_BYTES", MAX_SHARED_FILE_BYTES),
        ("MAX_FILE_NAME_BYTES", MAX_FILE_NAME_BYTES as u64),
        ("MAX_MIME_TYPE_BYTES", MAX_MIME_TYPE_BYTES as u64),
        ("MAX_FILE_PROVIDERS", MAX_FILE_PROVIDERS as u64),
        ("MAX_STATUS_LABEL_BYTES", MAX_STATUS_LABEL_BYTES as u64),
        ("MAX_STATUS_MESSAGE_BYTES", MAX_STATUS_MESSAGE_BYTES as u64),
        ("MAX_ARTIFACT_REFS", MAX_ARTIFACT_REFS as u64),
        ("PUBLIC_KEY_LEN", PUBLIC_KEY_LEN as u64),
        ("SIGNATURE_LEN", SIGNATURE_LEN as u64),
        ("DIGEST_LEN", DIGEST_LEN as u64),
        ("SHORT_ID_LEN", SHORT_ID_LEN as u64),
    ];
    for (name, value) in bounds {
        assert!(
            DOC.contains(name),
            "docs/protocol.md must name the constant `{name}`"
        );
        assert!(
            rendered_forms(*value).iter().any(|form| DOC.contains(form)),
            "docs/protocol.md names `{name}` but not its value {value}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tier-1 golden values must match the pinned fixtures.
// ---------------------------------------------------------------------------

#[test]
fn tier1_golden_values_match_fixtures() {
    // Recompute the golden CSB live from the same fixture the vectors use, then
    // assert the doc's stated byte-length and hex prefix track it — not a copied
    // constant, but the actual encoder output.
    let csb = fixtures::golden_event().to_csb();
    assert_eq!(
        csb.len(),
        242,
        "golden CSB length changed; update the doc if intended"
    );
    assert!(
        DOC.contains(&csb.len().to_string()),
        "docs/protocol.md must state the golden CSB length ({})",
        csb.len()
    );
    let csb_hex = hex::encode(&csb);
    let prefix = &csb_hex[..18]; // first 9 bytes — the doc's "a867..." anchor
    assert!(
        DOC.contains(prefix),
        "docs/protocol.md must carry the golden CSB hex prefix `{prefix}`"
    );

    // The Tier-1 pinned ids (asserted against the exact spike hex elsewhere in
    // this suite): the doc's worked example must quote them exactly.
    for golden in [
        fixtures::ROOM_ID_A_HEX,
        fixtures::ROOM_ID_B_HEX,
        fixtures::GOLDEN_EVENT_ID,
        fixtures::TAMPERED_EVENT_ID,
        fixtures::CROSS_ROOM_EVENT_ID,
    ] {
        assert!(
            DOC.contains(golden),
            "docs/protocol.md worked example must quote golden value `{golden}`"
        );
    }
}

// ---------------------------------------------------------------------------
// Vector-to-test map rows resolve to real tests; advertised commands are live.
// ---------------------------------------------------------------------------

#[test]
fn vector_map_rows_resolve_to_real_tests() {
    let table = vector_test_table(DOC);
    let fns = fn_names_from_table(&table);
    assert_eq!(
        fns.len(),
        20,
        "docs/protocol.md must map all 20 vectors (found {})",
        fns.len()
    );

    let sources = VECTOR_SOURCES.concat();
    for name in &fns {
        assert!(
            sources.contains(&format!("fn {name}")),
            "docs/protocol.md links `{name}`, but no such `fn` exists in the conformance modules"
        );
    }
}

#[test]
fn advertised_test_binaries_exist() {
    // The doc tells a reader to run these commands; a renamed binary would leave a
    // dead command in the reference.
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for bin in ["protocol_conformance", "golden_vectors"] {
        let cmd = format!("cargo test -p iroh-rooms-core --test {bin}");
        assert!(
            DOC.contains(&cmd),
            "docs/protocol.md must advertise `{cmd}`"
        );
        assert!(
            tests.join(format!("{bin}.rs")).exists(),
            "docs/protocol.md advertises `--test {bin}` but tests/{bin}.rs is missing"
        );
    }
}

// ---------------------------------------------------------------------------
// §6 registry: the doc's event-type table must be exactly the 10 wire strings.
// ---------------------------------------------------------------------------

#[test]
fn event_type_registry_table_matches_enum() {
    // Guard the hand-mirrored list: if this trips, the registry changed and both
    // this module and the doc's §6 table need review.
    assert_eq!(
        ALL_EVENT_TYPES.len(),
        10,
        "the MVP event-type registry has exactly 10 types"
    );

    let want: BTreeSet<&str> = ALL_EVENT_TYPES.iter().map(EventType::as_str).collect();

    // The §6 registry table's first column is the wire `event_type` string of each
    // row. Extract those cells and require them to be *exactly* the enum's set —
    // a missing row (undocumented type) or an invented row (a type the code does
    // not define) both fail.
    let table = registry_table(DOC);
    let got: BTreeSet<String> = leading_cells(&table)
        .into_iter()
        .filter(|c| is_event_type_shaped(c))
        .collect();
    let got_refs: BTreeSet<&str> = got.iter().map(String::as_str).collect();

    assert_eq!(
        got_refs,
        want,
        "docs/protocol.md §6 registry table must list exactly the {} EventType wire \
         strings — no missing, no invented rows",
        want.len()
    );
}

// ---------------------------------------------------------------------------
// The doc's hardcoded canonical top-level key order must be the encoder's order.
// ---------------------------------------------------------------------------

#[test]
fn canonical_key_order_matches_live_encoder() {
    // The doc hardcodes the canonical top-level key order (§3) as a fixed line so
    // implementers MAY hardcode it. Recompute that order from the *actual* encoder
    // by decoding the golden CSB, then require the doc to carry it verbatim — so
    // a reordering in the CBOR codec cannot silently invalidate the doc.
    let csb = fixtures::golden_event().to_csb();
    let decoded = decode_canonical(&csb).expect("golden CSB must decode as canonical CBOR");
    let CborValue::Map(pairs) = decoded else {
        panic!("golden CSB top-level value must be a CBOR map");
    };
    let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();

    // Sanity: the eight §2 signed fields, nothing more (guards a vacuous match on
    // an empty/short key list).
    assert_eq!(
        keys.len(),
        8,
        "the signed object must encode exactly the eight §2 fields, saw {}",
        keys.len()
    );

    let order_line = keys.join(", ");
    assert!(
        DOC.contains(&order_line),
        "docs/protocol.md must carry the encoder's canonical key order verbatim:\n  {order_line}"
    );
}

// ---------------------------------------------------------------------------
// Context-string byte-length annotations must equal the real constant lengths.
// ---------------------------------------------------------------------------

#[test]
fn context_string_byte_length_annotations_are_correct() {
    // §1/§5 annotate the two domain-separation strings that gate signatures with an
    // explicit byte length + "no NUL" (e.g. "(ASCII, 19 bytes, no NUL)"). A wrong
    // count here would mislead a from-scratch signer, so pin each annotation line
    // to the constant's real length. ROOMID/INVITE carry no byte count and are not
    // required to (no "no NUL" annotation), so they are correctly untouched.
    for (ctx, name) in [
        (EVENT_CONTEXT, "EVENT_CONTEXT"),
        (BIND_CONTEXT, "BIND_CONTEXT"),
    ] {
        let s = std::str::from_utf8(ctx).expect("context strings are ASCII");
        let want = format!("{} bytes", ctx.len());
        let annotated: Vec<&str> = DOC
            .lines()
            .filter(|line| line.contains(s) && line.contains("bytes, no NUL"))
            .collect();
        assert!(
            !annotated.is_empty(),
            "docs/protocol.md must annotate `{name}` with its byte length (`… bytes, no NUL`)"
        );
        for line in annotated {
            assert!(
                line.contains(&want),
                "docs/protocol.md annotates `{name}` with the wrong byte length \
                 (want `{want}`, its true length): {line}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Link integrity: every in-page anchor and relative file link must resolve —
// the doc-as-shipped equivalent of "click every link" (IR-0302 AC1/AC2: a
// reader who follows a section pointer or a cross-doc link must land
// somewhere real, not 404 against a renamed heading or moved file).
// ---------------------------------------------------------------------------

#[test]
fn anchor_links_resolve_to_a_real_heading() {
    let slugs: BTreeSet<String> = markdown_headings(DOC)
        .iter()
        .map(|h| github_slug(h))
        .collect();

    for target in markdown_link_targets(DOC) {
        let Some(anchor) = target.strip_prefix('#') else {
            continue;
        };
        assert!(
            slugs.contains(anchor),
            "docs/protocol.md links `(#{anchor})`, but no heading slugs to it \
             (known slugs: {slugs:?})"
        );
    }
}

#[test]
fn relative_file_links_resolve_on_disk() {
    let docs_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");

    for target in markdown_link_targets(DOC) {
        if target.starts_with('#')
            || target.starts_with("http://")
            || target.starts_with("https://")
        {
            continue;
        }
        // Strip an in-file fragment (`file.md#section`) before checking existence.
        let path_part = target.split('#').next().unwrap_or(target);
        let resolved = docs_dir.join(path_part);
        assert!(
            resolved.exists(),
            "docs/protocol.md links `({target})`, which resolves to `{}`, but that file \
             does not exist",
            resolved.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-link edits to existing docs resolve (spec §5): the discoverability
// half of "docs are linked" — a reader landing on README.md, the getting-
// started guide, or the spike must find a live pointer *into*
// `docs/protocol.md`, not just the reference pointing back out at them.
// ---------------------------------------------------------------------------

/// `README.md`, embedded so a stale/removed cross-link is caught at compile
/// time exactly like `DOC` above.
const README: &str = include_str!("../../../../README.md");

/// `docs/getting-started.md`, embedded for the same reason.
const GETTING_STARTED: &str = include_str!("../../../../docs/getting-started.md");

/// `PHASE-0-SPIKE.md`, embedded for the same reason.
const SPIKE: &str = include_str!("../../../../PHASE-0-SPIKE.md");

#[test]
fn readme_links_to_protocol_doc() {
    assert!(
        README.contains("docs/protocol.md"),
        "README.md must link to docs/protocol.md so the implementer reference is discoverable \
         next to docs/getting-started.md (spec §5)"
    );
}

#[test]
fn getting_started_links_to_protocol_doc() {
    assert!(
        GETTING_STARTED.contains("protocol.md"),
        "docs/getting-started.md must point readers at docs/protocol.md for the byte-level \
         protocol contract (spec §5)"
    );
}

#[test]
fn spike_event_protocol_section_points_to_protocol_doc() {
    let start = SPIKE
        .find("# Event Protocol")
        .expect("PHASE-0-SPIKE.md must have an Event Protocol section");
    assert!(
        SPIKE[start..].contains("docs/protocol.md"),
        "PHASE-0-SPIKE.md's Event Protocol section should point to docs/protocol.md as the \
         condensed implementer view (spec §5)"
    );
}

// ---------------------------------------------------------------------------
// Small, prose-tolerant Markdown helpers.
// ---------------------------------------------------------------------------

/// Every `](target)` substring in the doc — covers both `[text](#anchor)` and
/// `[text](relative/path.md)`. Markdown link syntax is distinctive enough that a
/// substring scan needs no fenced-code exclusion: no code block in this doc
/// contains a literal `](`.
fn markdown_link_targets(doc: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = doc;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        targets.push(&after[..end]);
        rest = &after[end + 1..];
    }
    targets
}

/// Every ATX heading's text (`#`..`######`), skipping fenced code blocks so a
/// shell comment (`# Full …`) inside a `sh` code fence is never mistaken for a
/// heading.
fn markdown_headings(doc: &str) -> Vec<String> {
    let mut in_fence = false;
    let mut headings = Vec::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) {
            if let Some(text) = trimmed[hashes..].strip_prefix(' ') {
                let text = text.trim();
                if !text.is_empty() {
                    headings.push(text.to_owned());
                }
            }
        }
    }
    headings
}

/// GitHub's heading-to-anchor slug: lowercase, drop everything but
/// ASCII-alphanumeric/space/hyphen/underscore, then turn spaces into hyphens.
/// Verified against this doc's own numbered headings (e.g. "8. Connect-time
/// authorization (blob & pipe)" -> `8-connect-time-authorization-blob--pipe`,
/// matching its own `[§8](#8-connect-time-authorization-blob--pipe)` links).
fn github_slug(heading: &str) -> String {
    let lower = heading.to_lowercase();
    let filtered: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .collect();
    filtered.replace(' ', "-")
}

/// The three *core* taxonomy tables only (Rejections + Ignored + Advisory flags),
/// deliberately excluding the CLI exit-category table that follows — it legitimately
/// names non-core codes (`peer_unauthorized`, `hash_mismatch`, ...).
fn core_taxonomy_tables(doc: &str) -> String {
    let start = doc
        .find("### Rejections")
        .expect("reason-code section must have a Rejections table");
    let rest = &doc[start..];
    let end = rest
        .find("### Surfacing")
        .expect("reason-code section must end at the CLI-surfacing subsection");
    rest[..end].to_owned()
}

/// The first data cell of every Markdown table row that looks like a taxonomy code
/// (lowercase ASCII + underscores). Header ("Code") and separator ("---") cells are
/// filtered out by the shape test.
fn leading_code_cells(md: &str) -> Vec<String> {
    md.lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .map(strip_cell)
        .filter(|token| is_code_shaped(token))
        .collect()
}

/// The §6 event-type registry table (between its section header and the
/// "Content schemas" subsection that follows it).
fn registry_table(doc: &str) -> String {
    let start = doc
        .find("## 6. MVP event-type registry")
        .expect("doc must have a §6 registry section");
    let rest = &doc[start..];
    let end = rest
        .find("### Content schemas")
        .expect("§6 registry section must be followed by the Content schemas subsection");
    rest[..end].to_owned()
}

/// The first data cell of every Markdown table row (whatever its shape). Callers
/// filter to the token shape they care about.
fn leading_cells(md: &str) -> Vec<String> {
    md.lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .map(strip_cell)
        .collect()
}

/// A token that could be a registry `event_type` wire string: non-empty, exactly
/// one `.`, all lowercase ASCII around it (e.g. `room.created`). Rejects the
/// header cell (`event_type`, no dot) and the separator (`---`).
fn is_event_type_shaped(token: &str) -> bool {
    token.bytes().filter(|&b| b == b'.').count() == 1
        && token.bytes().all(|b| b.is_ascii_lowercase() || b == b'.')
}

/// The `Test fn` column (third cell) of every vector-map row, restricted to the
/// `vector_*` function names.
fn fn_names_from_table(md: &str) -> Vec<String> {
    md.lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .filter_map(|line| line.split('|').nth(2))
        .map(strip_cell)
        .filter(|token| token.starts_with("vector_"))
        .collect()
}

/// The vector-to-test map table (between its header and the taxonomy-gate header).
fn vector_test_table(doc: &str) -> String {
    let start = doc
        .find("### Vector → test function → module")
        .expect("test-vectors section must have a vector map header");
    let rest = &doc[start..];
    let end = rest
        .find("### Taxonomy completeness")
        .expect("test-vectors section must end at the taxonomy-completeness gate");
    rest[..end].to_owned()
}

/// Trim whitespace and surrounding inline-code backticks from a table cell.
fn strip_cell(cell: &str) -> String {
    cell.trim().trim_matches('`').to_owned()
}

/// A token that could be a reason/flag code: non-empty, all lowercase ASCII or `_`,
/// starting with a letter.
fn is_code_shaped(token: &str) -> bool {
    !token.is_empty()
        && token.as_bytes()[0].is_ascii_lowercase()
        && token.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Plausible prose renderings of an integer bound: bare digits, comma-grouped
/// (`16,384`), and Rust underscore-grouped (`16_384`).
fn rendered_forms(n: u64) -> Vec<String> {
    let plain = n.to_string();
    let mut forms = vec![group_digits(&plain, ','), group_digits(&plain, '_'), plain];
    forms.sort();
    forms.dedup();
    forms
}

/// Group `digits` into threes from the right with `sep` (e.g. `16384` -> `16,384`).
fn group_digits(digits: &str, sep: char) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}
