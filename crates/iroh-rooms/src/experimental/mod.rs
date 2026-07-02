//! **Experimental (unstable API).** The online runtime: transport, admission,
//! the sync engine, the local event store, blob serve/fetch, and live-pipe
//! forwarding.
//!
//! Everything here has IO, a network, a clock, or a schema that can still
//! move — unlike the [`stable tier`](crate), it is **not** conformance-tested
//! against byte-exact golden vectors and may change on any release, including
//! a patch release. It is reachable only behind the `experimental` cargo
//! feature: a default-features build cannot even *name* a type in this
//! module.
//!
//! **Availability honesty (PRD §14).** Iroh Rooms is best-effort peer-to-peer:
//! there is **no central inbox and no guaranteed offline delivery**. Nothing
//! in [`session`] or [`sync`] implies a queue or guaranteed-delivery
//! capability that does not exist — an offline peer simply does not receive
//! events until it reconnects and syncs.
//!
//! Submodules mirror the online halves of the stable domain modules:
//! [`session`] (transport + admission + connection state), [`sync`] (the
//! sans-IO reconciliation engine + transport trait), [`store`] (local
//! persistence), [`blob`] ([`crate::files`]' runtime half), and
//! [`pipe_runtime`] ([`crate::pipes`]' runtime half).

pub mod blob;
pub mod pipe_runtime;
pub mod session;
pub mod store;
pub mod sync;
