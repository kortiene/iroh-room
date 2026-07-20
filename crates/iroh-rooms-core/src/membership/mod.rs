//! The deterministic membership fold and authorization layer (IR-0008): the
//! second **stateful** layer of the Room Event Plane, downstream of the stateless
//! [`event`](crate::event) validator (`PHASE-0-SPIKE.md` Membership & Ordering
//! §3/§5/§6/§7; spec `membership-fold-prototype.md`).
//!
//! It turns a set of [`ValidatedEvent`](crate::event::ValidatedEvent)s into:
//!
//! 1. a per-event **log-validity verdict** ([`Ingest`]), judged **only against
//!    the event's own causal ancestors** so it is identical on every peer
//!    regardless of arrival order (ancestor-stable, spec D3); and
//! 2. a deterministic **membership snapshot** ([`MembershipSnapshot`]) that the
//!    pipe/blob access decisions ([`blob_serve_allowed`], [`pipe_connect_allowed`])
//!    consult — the **current**-snapshot access boundary, kept rigorously separate
//!    from the ancestor-view log-validity boundary (spec D6).
//!
//! ## Load-bearing invariants (spike §0)
//!
//! * **One immutable admin** — the genesis signer is the sole authorization
//!   writer; membership never needs multi-writer state resolution.
//! * **Ancestor-stable validity** — every event is judged against its own fixed
//!   ancestors, never live state, so honest peers never permanently disagree.
//! * **Commutative causal fold** — monotonic admin authorizations + a per-subject
//!   causal-heads, **Removed-dominates** status rule + a deterministic
//!   **least-privilege** attribute merge are provably convergent.
//! * **Access uses the current snapshot** — a log-valid event from a
//!   since-removed member grants **zero** capabilities.
//!
//! The convergence guarantee, in its honest **same-set** form (spike §0):
//! any two peers holding the identical validated event set compute an equal
//! [`MembershipSnapshot`]. Equalizing the set (sync) and consuming the snapshot
//! (the planes) are sibling issues; this layer is the pure, conformance-tested
//! core they bolt onto without changing a fold rule.
//!
//! This module depends only on `event` types — no `store` feature, no I/O
//! (spec D1).

mod access;
mod fold;
mod model;

pub use access::{
    blob_serve_allowed, pipe_connect_allowed, BlobDecision, DenyReason, PipeDecision,
};
pub use fold::{AncestorView, Ingest, RoomMembership};
pub use model::{Member, MembershipSnapshot, Role, Status, MAX_ACTIVE_MEMBERS};
