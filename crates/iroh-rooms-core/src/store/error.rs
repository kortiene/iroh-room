//! The [`StoreError`] taxonomy for the `SQLite` event store.
//!
//! Hand-rolled (`Display` + [`std::error::Error`]), matching the crate's existing
//! error style (no `thiserror`). A known **duplicate is success**, not an error
//! (Event Protocol §6 step 11) — it is surfaced as
//! [`InsertOutcome::Duplicate`](super::model::InsertOutcome), never here.

use core::fmt;

use crate::event::reject::RejectReason;

/// An error from the event store.
///
/// The store persists only pre-validated events and never panics on stored bytes
/// (spec §9): a corrupt/truncated `wire` row surfaces as [`StoreError::Decode`]
/// or [`StoreError::Integrity`] from [`rebuild`](super::EventStore::rebuild),
/// never an `unwrap`/slice panic.
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    /// An underlying `rusqlite`/`SQLite` error (open, migrate, prepare, step).
    Sqlite(rusqlite::Error),
    /// A stored `wire` blob failed to decode during `rebuild` — i.e. on-disk
    /// corruption of an otherwise-authoritative row (spec D5).
    Decode(RejectReason),
    /// A recomputed value disagreed with a stored one: the `BLAKE3(wire.signed)`
    /// re-derivation did not equal the row's `event_id` key, a derived column was
    /// out of range, or a stored enum/string was unrecognized (spec §9).
    Integrity(String),
    /// Schema migration failed (e.g. an unsupported existing `user_version`).
    Migration(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Self::Decode(r) => write!(f, "stored event failed to decode (corruption): {r}"),
            Self::Integrity(msg) => write!(f, "store integrity violation: {msg}"),
            Self::Migration(msg) => write!(f, "store migration failed: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Decode(r) => Some(r),
            Self::Integrity(_) | Self::Migration(_) => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl StoreError {
    /// Construct an [`StoreError::Integrity`] from a message.
    pub(crate) fn integrity(msg: impl Into<String>) -> Self {
        Self::Integrity(msg.into())
    }
}
