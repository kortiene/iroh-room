//! `spike-nat` — the IR-0012 Gate-A real-NAT hole-punching measurement harness.
//!
//! Gate A is the one load-bearing Phase-0 assumption with no measured evidence:
//! that two iroh endpoints behind real, separate NATs can establish a **direct**
//! QUIC connection, or cleanly **fall back to relay**. A LAN or loopback demo
//! cannot exercise NAT traversal and "will lie to you about it"
//! (`PHASE-0-SPIKE.md` Day 1). This crate is the purpose-built *measurement* tool
//! that closes the gate empirically: a bare `iroh::Endpoint` echo probe that
//! reports, per scenario and per direction, the **path type actually achieved**
//! (read off iroh — never inferred from latency), time-to-first-byte, RTT and
//! throughput.
//!
//! It is a **throwaway spike** (mirrors [`spike-blobs`]): isolated from the
//! shipping crates' dependency tree, kept in the workspace so CI proves it builds
//! and its loopback self-check passes. CI **cannot** prove NAT traversal — that
//! is the manual two-host run documented in [`NOTES.md`]. The durable outputs are
//! the committed per-run JSON under `results/`, the rolled-up `results.md` table,
//! and the Gate-A findings block liftable into the Gate E go/no-go memo (#15).
//!
//! - [`probe`] — endpoint bring-up, the echo protocol, dial-and-measure, and the
//!   path-type settle-and-sample classification.
//! - [`report`] — the [`report::ProbeResult`] record (the §5 measurement contract)
//!   plus its JSON and Markdown emitters.
//!
//! [`spike-blobs`]: https://docs.rs/spike-blobs
//! [`NOTES.md`]: https://github.com/kortiene/iroh-room/blob/main/crates/spike-nat/NOTES.md

pub mod probe;
pub mod report;
