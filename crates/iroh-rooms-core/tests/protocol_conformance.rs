//! Executable protocol conformance suite for `PHASE-0-SPIKE.md` Protocol Test
//! Vectors §1–§20 plus the §8 Rejection / Flag Taxonomy (IR-0003 / #7).
//!
//! This is the single, §-indexed, traceable conformance binary the spike's
//! Gate B / Gate D require. It consolidates the byte-exact stateless vectors,
//! pins the previously-unreproduced stateful fixture-log golden ids, and adds a
//! machine-checked taxonomy completeness gate. See [`conformance`] (the module
//! doc-comment) for the full vector → test → reason-code traceability table.
//!
//! Run with:
//! `cargo test -p iroh-rooms-core --test protocol_conformance --all-features`
//!
//! The suite is fast, network-free, and deterministic: every key is seed-derived
//! and every clock is injected (`ValidationContext.now_ms`, `created_at`), so no
//! vector reads a wall clock or draws entropy.

mod conformance;
