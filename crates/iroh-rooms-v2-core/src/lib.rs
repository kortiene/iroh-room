//! Pure v2 cryptographic and deterministic-algorithm core (spec
//! `v2-crypto-core-crate.md`, issue #140).
//!
//! # Crate invariants
//!
//! This crate is the **pure** v2 foundation. It owns only deterministic protocol
//! logic and MUST NOT contain, transitively depend on, or import:
//!
//! - network transports, ALPN constants, router accept loops (`iroh`,
//!   `iroh-blobs`, `iroh-gossip`);
//! - async runtimes (`tokio`/`tokio::spawn`);
//! - storage (`rusqlite`, `SQLite`, replicas, receipts);
//! - group encryption / key ratchets / payload encryption;
//! - migration tooling or deployment-model commitments.
//!
//! The `banned_dependencies` test machine-checks the absence of these crates in
//! the dependency tree.
//!
//! # Trust boundary
//!
//! Every signed v2 record (spec D2) crosses a single trust boundary expressed as
//! canonical signed bytes (`CSB`):
//!
//! 1. a logical body struct serializes to [`cbor::CborValue`];
//! 2. `CSB = cbor::encode(body)` under the deterministic profile;
//! 3. the signing message is `DOMAIN_SIGN_CONTEXT || CSB` (see [`domain`]);
//! 4. the record id is `BLAKE3(DOMAIN_ID_CONTEXT || CSB)`;
//! 5. the wire/storage envelope preserves `CSB` byte-for-byte.
//!
//! Receivers verify the exact bytes they received; they never re-serialize before
//! signature verification.
//!
//! # Normative-source assumptions (open questions OQ-1..OQ-9)
//!
//! `#134 §6–§9` normative text is not present in this checkout. Per spec §13
//! ("Block code on missing normative text; keep guessed names only in this spec
//! as candidates"), the following safe, explicit assumptions are recorded in code
//! and are the candidates the spec itself proposes:
//!
//! - **OQ-1 (domain strings):** the candidate contexts in spec D3 are used
//!   verbatim, centralized in [`domain`], and pinned by compile-time tests.
//! - **OQ-2 (key model):** a single Ed25519 signing key per principal; the public
//!   key is the [`MemberId`]/[`PrincipalId`] (`keys`).
//! - **OQ-3/#148 (governance actions):** a closed `GovernanceAction` set with an
//!   explicit, default-deny authorization engine (`governance::authz`).
//! - **OQ-4 (state root):** the state root commits to accepted governance state
//!   *and* unresolved fork evidence (spec §11: "commits to all state that affects
//!   authorization").
//! - **OQ-7 (Merkle map):** the sparse BLAKE3 Merkle map of spec D7.
//! - **OQ-8 (content registry):** the registry in the sibling spec
//!   `content-and-moderation-event-schemas.md` §4 D1 (named as the nearest local
//!   planning input by spec D8 / §14 assumption 4).
//!
//! Any of these may change when `#134` lands; such changes are isolated to the
//! modules above and pinned by focused tests.

#![forbid(unsafe_code)]

pub mod cbor;
pub mod content;
pub mod domain;
pub mod error;
pub mod governance;
pub mod ids;
pub mod keys;
pub mod member;
pub mod schema;
pub mod signed;

pub use error::Reject;

/// Re-export of the principal/identity public-key type used across the crate.
pub use ids::{MemberId, PrincipalId};

/// Re-export of the #134 §6.3 frozen v2 identifier newtypes (issue #146).
pub use ids::{CheckpointId, CommunityId, EventId, GovernanceId, ReplicaId, StreamId};

/// The canonical CBOR value type (spec §6.2).
pub use cbor::CborValue;
