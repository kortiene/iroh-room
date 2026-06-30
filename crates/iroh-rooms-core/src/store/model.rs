//! Value types the store returns: [`InsertOutcome`], [`InsertStats`], and the
//! read-side [`StoredEvent`] projection.

use crate::event::content::EventType;
use crate::event::ids::{EventId, RoomId};
use crate::event::wire::WireEvent;

/// The result of a single idempotent [`insert`](super::EventStore::insert).
///
/// A known duplicate is **not** an error (Event Protocol §6 step 11): the first
/// validly-signed copy wins, and inserting it 1× or 1000× yields identical state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// The event was new and is now stored exactly once.
    Inserted,
    /// The `event_id` was already present; the call was a no-op.
    Duplicate,
}

impl InsertOutcome {
    /// Whether this outcome stored a new row.
    #[must_use]
    pub fn is_inserted(self) -> bool {
        matches!(self, Self::Inserted)
    }
}

/// Counts returned by a bulk [`insert_all`](super::EventStore::insert_all).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InsertStats {
    /// Number of newly-stored events.
    pub inserted: u64,
    /// Number of ignored duplicates.
    pub duplicate: u64,
}

impl InsertStats {
    /// Fold one [`InsertOutcome`] into the running counts.
    pub(crate) fn record(&mut self, outcome: InsertOutcome) {
        match outcome {
            InsertOutcome::Inserted => self.inserted += 1,
            InsertOutcome::Duplicate => self.duplicate += 1,
        }
    }
}

/// A read-side projection of a stored event.
///
/// `wire` is re-decoded verbatim from the authoritative stored bytes; the other
/// fields are the indexed derived cache (spec D4). `sender_id` / `device_id` /
/// `created_at` are reachable via [`StoredEvent::wire`] → the decoded
/// `SignedEvent`, so they are not duplicated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The dedup key: `BLAKE3-256(wire.signed)`.
    pub event_id: EventId,
    /// The verbatim transport envelope, re-decoded from the stored bytes.
    pub wire: WireEvent,
    /// The room this event belongs to.
    pub room_id: RoomId,
    /// The registered event type.
    pub event_type: EventType,
    /// Derived Lamport timestamp; `None` while the event is causally incomplete
    /// (a parent is missing), per spec §6 / §2.1.
    pub lamport: Option<u64>,
    /// Derived admin self-parent-chain sequence; `Some` only for events authored
    /// by the room admin once the genesis is present (spec §6 / Membership §0).
    pub admin_seq: Option<u64>,
}
