//! Content event body validation (spec §4 D8 / §9.2 / #152, source registry:
//! `specs/content-and-moderation-event-schemas.md` §4 D1).
//!
//! The v2 content-kind registry is **closed**: an unknown `kind` is rejected
//! ([`crate::Reject::UnknownContentKind`]), never ignored (the §6.4 rule). Each
//! registered kind has a strict `content` schema (exact key set, required/
//! optional, types, byte/count caps, enums). This layer is **body-only**: no
//! blob fetch, no stream transport, no encryption (spec §3.2 out-of-scope).
//!
//! # Two schemas
//!
//! - [`body::ContentEventBody`] is the normative #134 §9.2 schema (issue #152):
//!   the single accepted v2 content wire format, with the concrete exact-byte
//!   envelope in [`event`] (`ContentEvent` / `VerifiedContentEvent`).
//! - [`provisional::ProvisionalContentEventBody`] is the pre-#152 provisional
//!   schema, retained ONLY to keep the frozen #153 golden vectors byte-stable.
//!   It is not normative and cannot be decoded as normative bytes.

pub mod body;
pub mod event;
pub mod provisional;
pub mod registry;
pub mod validate;

pub use body::{ContentEventBody, CONTENT_EVENT_VERSION, MAX_CONTENT_REFERENCES};
pub use event::{
    seal_content_event, validate_device_chain_link, verify_content_event, ContentEvent,
    VerifiedContentEvent,
};
pub use registry::ContentKind;
pub use validate::{validate_body, validate_content};
