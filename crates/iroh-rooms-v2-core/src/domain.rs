//! Versioned domain-separation constants for every v2 hash/signature/Merkle
//! boundary (spec §4 D3, issue #146).
//!
//! Every cryptographic boundary takes an explicit context string so a signature
//! or hash valid in one role can never be replayed in another. These are the
//! **candidate** contexts from spec D3, centralized here and pinned by
//! compile-time tests; OQ-1 tracks reconciliation with the `#134` July 2026
//! decision record. Changing any of these is a protocol-breaking change that
//! must be paired with re-freezing the golden vectors.

/// A domain-separation context string. Every boundary in this module is ASCII
/// with no embedded NUL.
pub struct Domain;

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
/// Domain prefix for a Merkle internal node hash.
pub const MERKLE_NODE: &[u8] = b"iroh-rooms:v2:merkle:node:v1";
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
    MERKLE_NODE,
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
}
