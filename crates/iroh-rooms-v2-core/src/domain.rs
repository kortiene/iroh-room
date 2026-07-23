//! Domain-separation constants for every v2 hash/signature/Merkle boundary
//! (spec §4 D3, issue #146).
//!
//! Every cryptographic boundary takes an explicit context string so a signature
//! or hash valid in one role can never be replayed in another.
//!
//! # Two layers
//!
//! 1. **Frozen `#134 §6.2` constants** (the `FROZEN_*` block + the bare names
//!    like [`COMMUNITY`], [`GOVERNANCE_ENTRY`], …) are the normative v2
//!    domain-separation strings. They are byte-pinned by tests in this module
//!    and by the identifier golden vectors. Changing any of them is a
//!    protocol-breaking change that requires a fixture schema bump.
//! 2. **Legacy candidate contexts** (`GOVERNANCE_ENTRY_SIGN`, `ROOM_ID`, …)
//!    remain as private compatibility aliases so already-landed child modules
//!    and the `#153` signed-record golden vectors keep compiling. They are NOT
//!    normative; new code must use the frozen `#134 §6.2` names. A follow-up
//!    reconciliation pass will migrate the child modules and retire them.

/// A domain-separation context string. Every boundary in this module is ASCII
/// with no embedded NUL.
pub struct Domain;

// ---------------------------------------------------------------------------
// Frozen #134 §6.2 domain-separation strings (issue #146). NORMATIVE.
//
// Exactly the eleven ASCII strings the issue freezes, in `iroh-room-v2/<kind>`
// form. One domain per semantic boundary; per spec D2 the same domain is used
// for both id derivation (`BLAKE3(domain || preimage)`) and signing messages
// (`domain || canonical_record_bytes`) unless a future #134 revision declares
// otherwise. Public tests below pin every byte.
// ---------------------------------------------------------------------------

/// Domain for the community/room identity (spec §6.2 / §6.3 `CommunityId`).
pub const COMMUNITY: &[u8] = b"iroh-room-v2/community";
/// Domain for a governance log entry (spec §6.2 / §6.3 `GovernanceId`).
pub const GOVERNANCE_ENTRY: &[u8] = b"iroh-room-v2/governance-entry";
/// Domain for a governance approval signature/id (spec §6.2).
pub const GOVERNANCE_APPROVAL: &[u8] = b"iroh-room-v2/governance-approval";
/// Domain for a content event (spec §6.2 / §6.3 `EventId`).
pub const CONTENT_EVENT: &[u8] = b"iroh-room-v2/content-event";
/// Domain for a member Merkle-map leaf hash (spec §6.2 / #151).
pub const MEMBER_LEAF: &[u8] = b"iroh-room-v2/member-leaf";
/// Domain for a Merkle-map internal-node hash (spec §6.2 / #151).
pub const MERKLE_NODE: &[u8] = b"iroh-room-v2/merkle-node";
/// Domain for the governance state root (spec §6.2 / #147).
pub const GOVERNANCE_STATE: &[u8] = b"iroh-room-v2/governance-state";
/// Domain for a replica receipt / `ReplicaId` boundary (spec §6.2 / §6.3).
pub const REPLICA_RECEIPT: &[u8] = b"iroh-room-v2/replica-receipt";
/// Domain for a governance checkpoint (spec §6.2 / §6.3 `CheckpointId`).
pub const GOVERNANCE_CHECKPOINT: &[u8] = b"iroh-room-v2/governance-checkpoint";
/// Domain for a stream checkpoint (spec §6.2 / §6.3 `CheckpointId`, stream kind).
pub const STREAM_CHECKPOINT: &[u8] = b"iroh-room-v2/stream-checkpoint";
/// Domain for a migration record (spec §6.2).
pub const MIGRATION: &[u8] = b"iroh-room-v2/migration";

/// The complete frozen `#134 §6.2` domain set, as `(name, bytes)` pairs. Used
/// by the byte-pin/completeness test so a new domain cannot land without being
/// added here, and a removed domain cannot disappear silently.
pub const ALL_V2_DOMAINS: &[(&str, &[u8])] = &[
    ("COMMUNITY", COMMUNITY),
    ("GOVERNANCE_ENTRY", GOVERNANCE_ENTRY),
    ("GOVERNANCE_APPROVAL", GOVERNANCE_APPROVAL),
    ("CONTENT_EVENT", CONTENT_EVENT),
    ("MEMBER_LEAF", MEMBER_LEAF),
    ("MERKLE_NODE", MERKLE_NODE),
    ("GOVERNANCE_STATE", GOVERNANCE_STATE),
    ("REPLICA_RECEIPT", REPLICA_RECEIPT),
    ("GOVERNANCE_CHECKPOINT", GOVERNANCE_CHECKPOINT),
    ("STREAM_CHECKPOINT", STREAM_CHECKPOINT),
    ("MIGRATION", MIGRATION),
];

// ---------------------------------------------------------------------------
// Legacy candidate contexts (pre-#146 scaffolding). Private compatibility
// aliases used only by already-landed child modules and the #153 signed-record
// golden vectors. New code MUST use the frozen names above.
// ---------------------------------------------------------------------------

// --- Governance entry (#147) ------------------------------------------------
/// Signing message prefix for a governance entry signature.
pub const GOVERNANCE_ENTRY_SIGN: &[u8] = b"iroh-rooms:v2:governance-entry:sign:v1";
/// Domain prefix for governance entry id derivation.
pub const GOVERNANCE_ENTRY_ID: &[u8] = b"iroh-rooms:v2:governance-entry:id:v1";

// --- Governance approval (#147) ---------------------------------------------
/// Signing message prefix for a governance approval signature.
pub const GOVERNANCE_APPROVAL_SIGN: &[u8] = b"iroh-rooms:v2:governance-approval:sign:v1";
/// Domain prefix for governance approval id derivation.
pub const GOVERNANCE_APPROVAL_ID: &[u8] = b"iroh-rooms:v2:governance-approval:id:v1";

// --- Content event (#152) ----------------------------------------------------
/// Signing message prefix for a content event signature.
pub const CONTENT_EVENT_SIGN: &[u8] = b"iroh-rooms:v2:content-event:sign:v1";
/// Domain prefix for content event id derivation.
pub const CONTENT_EVENT_ID: &[u8] = b"iroh-rooms:v2:content-event:id:v1";

// --- Room / space (#146) -----------------------------------------------------
/// Domain prefix for room/space id derivation.
pub const ROOM_ID: &[u8] = b"iroh-rooms:v2:room-id:v1";

// --- Governance state root (#147) -------------------------------------------
/// Domain prefix for the governance state root.
pub const GOVERNANCE_STATE_ROOT: &[u8] = b"iroh-rooms:v2:governance-state-root:v1";

// --- Checkpoint / snapshot (#150) -------------------------------------------
/// Signing message prefix for a checkpoint signature.
pub const CHECKPOINT_SIGN: &[u8] = b"iroh-rooms:v2:checkpoint:sign:v1";
/// Domain prefix for the snapshot hash.
pub const SNAPSHOT_HASH: &[u8] = b"iroh-rooms:v2:snapshot-hash:v1";

// --- Merkle map (D7, #151) ---------------------------------------------------
/// Domain prefix for a Merkle empty node (depth-specific).
pub const MERKLE_EMPTY: &[u8] = b"iroh-rooms:v2:merkle:empty:v1";
/// Domain prefix for a Merkle leaf hash.
pub const MERKLE_LEAF: &[u8] = b"iroh-rooms:v2:merkle:leaf:v1";
/// Domain prefix for a Merkle internal node hash (legacy candidate; the
/// `#134 §6.2` frozen name is [`MERKLE_NODE`]).
pub const LEGACY_MERKLE_NODE: &[u8] = b"iroh-rooms:v2:merkle:node:v1";
/// Domain prefix for a Merkle map key derivation.
pub const MERKLE_KEY: &[u8] = b"iroh-rooms:v2:merkle:key:v1";

// --- Fork resolution (#149) --------------------------------------------------
/// Signing message prefix for a fork resolution signature.
pub const FORK_RESOLVE_SIGN: &[u8] = b"iroh-rooms:v2:fork-resolve:sign:v1";
/// Domain prefix for a fork resolution id derivation.
pub const FORK_RESOLVE_ID: &[u8] = b"iroh-rooms:v2:fork-resolve:id:v1";

/// Compute a domain-separated BLAKE3-256 digest: `BLAKE3(context || payload)`.
///
/// The `context` is one of the `*_ID`/`*_ROOT`/`*_HASH`/`MERKLE_*` constants
/// above — never a signature context. Pinning the context at the call site keeps
/// wrong-domain use a type-level concern (callers reference the named constant).
#[must_use]
pub fn blake3_domain(context: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(context);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

/// Build an Ed25519 signing message: `SIGN_CONTEXT || payload` (spec D2 step 3).
#[must_use]
pub fn signing_message(sign_context: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(sign_context.len() + payload.len());
    msg.extend_from_slice(sign_context);
    msg.extend_from_slice(payload);
    msg
}

/// SHA-256-style guard: every context is ASCII with no embedded NUL. (The values
/// are compile-time constants; this asserts at test time that no accidental
/// control byte slipped in.)
#[cfg(test)]
const ALL_CONTEXTS: &[&[u8]] = &[
    GOVERNANCE_ENTRY_SIGN,
    GOVERNANCE_ENTRY_ID,
    GOVERNANCE_APPROVAL_SIGN,
    GOVERNANCE_APPROVAL_ID,
    CONTENT_EVENT_SIGN,
    CONTENT_EVENT_ID,
    ROOM_ID,
    GOVERNANCE_STATE_ROOT,
    CHECKPOINT_SIGN,
    SNAPSHOT_HASH,
    MERKLE_EMPTY,
    MERKLE_LEAF,
    LEGACY_MERKLE_NODE,
    MERKLE_KEY,
    FORK_RESOLVE_SIGN,
    FORK_RESOLVE_ID,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every context is ASCII, non-empty, NUL-free, and ends with `:v1`.
    #[test]
    fn contexts_are_well_formed() {
        assert_eq!(
            ALL_CONTEXTS.len(),
            16,
            "spec D3 lists exactly 15 boundaries; v2 adds fork-resolve id"
        );
        let mut seen: Vec<&[u8]> = Vec::new();
        for ctx in ALL_CONTEXTS {
            assert!(!ctx.is_empty(), "empty context");
            assert!(ctx.ends_with(b":v1"), "context {ctx:?} must end :v1");
            for &b in *ctx {
                assert!(b.is_ascii(), "non-ascii byte {b} in context");
                assert_ne!(b, 0, "NUL in context");
            }
            assert!(seen.iter().all(|s| *s != *ctx), "duplicate context {ctx:?}");
            seen.push(*ctx);
        }
    }

    /// Pin the exact bytes of the two contexts the golden vectors will freeze
    /// first (spec D3 / OQ-1). If these drift, every signature/id breaks.
    #[test]
    fn governance_entry_contexts_are_pinned() {
        assert_eq!(
            GOVERNANCE_ENTRY_SIGN,
            b"iroh-rooms:v2:governance-entry:sign:v1"
        );
        assert_eq!(GOVERNANCE_ENTRY_ID, b"iroh-rooms:v2:governance-entry:id:v1");
    }

    /// Wrong-domain hashes must differ: the same payload under two contexts
    /// produces two distinct digests.
    #[test]
    fn domain_separation_distinguishes_payloads() {
        let payload = b"identical payload";
        let a = blake3_domain(GOVERNANCE_STATE_ROOT, payload);
        let b = blake3_domain(SNAPSHOT_HASH, payload);
        assert_ne!(a, b);
    }

    #[test]
    fn signing_message_is_context_then_payload() {
        let msg = signing_message(GOVERNANCE_ENTRY_SIGN, b"csb");
        let mut expect = GOVERNANCE_ENTRY_SIGN.to_vec();
        expect.extend_from_slice(b"csb");
        assert_eq!(msg, expect);
    }

    // ------------------------------------------------------------------------
    // #134 §6.2 frozen domain-separation byte-pinning (issue #146 acceptance).
    // ------------------------------------------------------------------------

    /// Every frozen domain equals the exact `iroh-room-v2/...` byte string from
    /// #134 §6.2. One assertion per domain so a drift message names the culprit.
    #[test]
    fn frozen_v2_domains_are_exact_bytes() {
        assert_eq!(COMMUNITY, b"iroh-room-v2/community");
        assert_eq!(GOVERNANCE_ENTRY, b"iroh-room-v2/governance-entry");
        assert_eq!(GOVERNANCE_APPROVAL, b"iroh-room-v2/governance-approval");
        assert_eq!(CONTENT_EVENT, b"iroh-room-v2/content-event");
        assert_eq!(MEMBER_LEAF, b"iroh-room-v2/member-leaf");
        assert_eq!(MERKLE_NODE, b"iroh-room-v2/merkle-node");
        assert_eq!(GOVERNANCE_STATE, b"iroh-room-v2/governance-state");
        assert_eq!(REPLICA_RECEIPT, b"iroh-room-v2/replica-receipt");
        assert_eq!(GOVERNANCE_CHECKPOINT, b"iroh-room-v2/governance-checkpoint");
        assert_eq!(STREAM_CHECKPOINT, b"iroh-room-v2/stream-checkpoint");
        assert_eq!(MIGRATION, b"iroh-room-v2/migration");
    }

    /// Exactly the eleven #134 §6.2 domains are registered in `ALL_V2_DOMAINS`.
    #[test]
    fn frozen_v2_domain_set_is_complete_and_unique() {
        assert_eq!(
            ALL_V2_DOMAINS.len(),
            11,
            "#134 §6.2 freezes exactly eleven domain strings"
        );
        let mut seen: Vec<&[u8]> = Vec::new();
        for (name, bytes) in ALL_V2_DOMAINS {
            // ASCII, non-empty, no NUL/control bytes.
            assert!(!bytes.is_empty(), "{name}: empty domain");
            for &b in *bytes {
                assert!(b.is_ascii(), "{name}: non-ascii byte {b}");
                assert_ne!(b, 0, "{name}: NUL byte");
                assert!(b.is_ascii_graphic(), "{name}: control/non-graphic byte {b}");
            }
            // Form check: singular `iroh-room-v2/` prefix (no plural, no colon,
            // no `:v1` suffix — that was the legacy candidate shape).
            assert!(
                bytes.starts_with(b"iroh-room-v2/"),
                "{name}: must start with `iroh-room-v2/`"
            );
            assert!(
                !bytes.contains(&b':'),
                "{name}: frozen domains use `/`, never `:`"
            );
            assert!(
                !bytes.ends_with(b":v1"),
                "{name}: frozen domains are unversioned (no `:v1`)"
            );
            // Uniqueness.
            assert!(!seen.contains(bytes), "{name}: duplicate domain {bytes:?}");
            seen.push(*bytes);
        }
    }

    /// Regression fence: no frozen domain may equal an old candidate string.
    /// Catches a copy-paste that pulls the pre-#146 `iroh-rooms:v2:...:v1` form
    /// back into the frozen set.
    #[test]
    fn frozen_v2_domains_are_not_legacy_candidates() {
        const LEGACY_EXAMPLES: &[&[u8]] = &[
            b"iroh-rooms:v2:governance-entry:sign:v1",
            b"iroh-rooms:v2:governance-entry:id:v1",
            b"iroh-rooms:v2:room-id:v1",
            b"iroh-rooms:v2:merkle:node:v1",
        ];
        for (_, frozen) in ALL_V2_DOMAINS {
            for legacy in LEGACY_EXAMPLES {
                assert_ne!(
                    frozen, legacy,
                    "frozen domain {frozen:?} collides with a legacy candidate"
                );
            }
        }
    }

    /// Domain separation has teeth: the same preimage under two frozen domains
    /// yields two distinct digests.
    #[test]
    fn frozen_domains_separate_payloads() {
        let preimage = b"common-preimage";
        let a = blake3_domain(COMMUNITY, preimage);
        let b = blake3_domain(GOVERNANCE_ENTRY, preimage);
        assert_ne!(a, b);
    }
}
