//! **Iroh Rooms Rust SDK** — the supported public entry point for driving an
//! Iroh Rooms room from a Rust program (issue #36 / IR-0301).
//!
//! Iroh Rooms is a local-first, peer-to-peer collaboration runtime: two humans
//! and one agent exchange signed messages, share verified files, expose a
//! private live TCP pipe, and post status updates without a central
//! application server (see the repository `README.md` and
//! `docs/getting-started.md` — the demo these `examples/` mirror).
//!
//! This crate does **not** implement any of that runtime itself. It is a
//! **curated, documented, stability-tiered façade** over the already-shipped
//! `iroh-rooms-core` and `iroh-rooms-net` crates, organized into five domain
//! modules — [`identity`], [`room`], [`events`], [`files`], [`pipes`] — plus
//! an [`experimental`] namespace for the online runtime. A re-exported type is
//! *the same type* as its `core`/`net` original (re-export, not re-wrap), so
//! mixing this façade with a direct `core`/`net` dependency never produces two
//! incompatible copies of the same type.
//!
//! # Stability tiers
//!
//! * **stable** (default features) — the deterministic, conformance-tested,
//!   byte-stable protocol layer: event authoring ([`events`], [`room`],
//!   [`files`], [`pipes`] builders), the [`events::WireEvent`] /
//!   [`events::SignedEvent`] model, [`events::validate_wire_bytes`], the
//!   membership fold + access predicates ([`room`]), and the ticket codec
//!   ([`room::RoomInviteTicket`]). This is the layer proven byte-exact by the
//!   protocol conformance suite (issue #7 / IR-0003).
//! * **experimental** (`--features experimental`) — the online runtime:
//!   transport, sync engine, local event store, blob serve/fetch, and
//!   live-pipe forwarding, reachable only via [`experimental`]. It may change
//!   on any release; every item's doc opens with **`Experimental (unstable
//!   API).`** and the module carries a `doc(cfg(...))` badge. A
//!   default-features build cannot even *name* an experimental type — the
//!   feature gate is the load-bearing marker, the namespace and doc marker are
//!   belt-and-suspenders (spec D4).
//!
//! The tiering principle is *not* arbitrary: it aligns the stability promise
//! with what is actually pure/deterministic vs. IO-bearing. It also means the
//! stable surface implies **no post-MVP capability** — no multi-device, no
//! call plane, no availability-layer (always-on node / archive peer /
//! pinning) API. Those all live in (or below) the online runtime, which is
//! explicitly experimental and narrowly shaped. See `PRD.v0.3.md` §7.3/§9.4/
//! §13.4/§13.5/§19 Phase 5 for what is deliberately out of scope.
//!
//! # Versioning
//!
//! This crate starts at `0.1.0` and follows a plain semver-for-0.x policy:
//! within `0.x`, the **stable** tier changes only on a minor bump (with a
//! `CHANGELOG.md` entry and a deprecation window where feasible); the
//! **experimental** tier may change on any release. This is "stable-ish" —
//! honestly scoped for a pre-1.0 developer preview, not a 1.0 guarantee.
//!
//! # Getting started
//!
//! Start with [`prelude`] for the most-used stable types, then the
//! `examples/` directory (`cargo run --example 01_identity`,
//! `cargo run --example offline_author_and_validate`) for runnable,
//! copy-pasteable walkthroughs mirroring `docs/getting-started.md`. Online
//! examples (`03`–`07`) require `--features experimental` and two live peers
//! to run; they always compile in CI, mirroring the crate's own
//! `#[ignore]`-gated online test tier. `examples/example_agent/` (issue #39 /
//! IR-0304) is a runnable, adapt-me-as-a-template agent driven by real
//! command-line arguments — start with its co-located `README.md`.
//!
//! # Errors
//!
//! This SDK surfaces the existing typed error enums as-is — [`events::RejectReason`]
//! and [`room::TicketError`] in the stable tier; `StoreError` / `SyncError` /
//! `BlobError` / `PipeError` in [`experimental`]. There is no unifying
//! `SdkError` — that would hide the taxonomy the CLI's own error mapping
//! (IR-0110) deliberately branches on.
//!
//! # Availability honesty
//!
//! Iroh Rooms is best-effort peer-to-peer: there is no central inbox and no
//! guaranteed offline delivery (PRD §14). Nothing in this SDK's shape implies
//! otherwise — see [`experimental::session`] and [`experimental::sync`] for
//! the online runtime this disclaimer applies to.

#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod events;
pub mod files;
pub mod identity;
pub mod pipes;
pub mod room;

#[cfg(feature = "experimental")]
#[cfg_attr(docsrs, doc(cfg(feature = "experimental")))]
pub mod experimental;

/// A glob of the most-used **stable** types, so `use iroh_rooms::prelude::*;`
/// covers the common case of authoring, validating, and folding events.
///
/// The prelude never re-exports an [`experimental`] item — a consumer of only
/// the prelude can never accidentally pull an unstable type into scope (spec
/// D6).
///
/// ```
/// use iroh_rooms::prelude::*;
///
/// let identity = SigningKey::generate();
/// let _ = identity.identity_key();
/// ```
pub mod prelude {
    pub use crate::events::{build_message_text, EventId, RejectReason, WireEvent};
    pub use crate::identity::{IdentityKey, SigningKey};
    pub use crate::room::{
        build_member_invited, build_member_joined, build_room_created, Role, RoomId,
        RoomInviteTicket, Status,
    };
}
