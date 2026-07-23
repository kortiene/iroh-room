//! Content event body validation (spec §4 D8 / §9.2 / #152, source registry:
//! `specs/content-and-moderation-event-schemas.md` §4 D1).
//!
//! The v2 content-kind registry is **closed**: an unknown `kind` is rejected
//! ([`crate::Reject::UnknownContentKind`]), never ignored (the §6.4 rule). Each
//! registered kind has a strict `body` schema (exact key set, required/optional,
//! types, byte/count caps, enums). This layer is **body-only**: no blob fetch,
//! no stream transport, no encryption (spec §3.2 out-of-scope).

pub mod body;
pub mod registry;
pub mod validate;

pub use body::{ContentBodyKind, ContentEventBody};
pub use registry::ContentKind;
pub use validate::validate_body;
