//! Value types the store returns: [`InsertOutcome`], [`InsertStats`], the
//! read-side [`StoredEvent`] projection, and the schema-v2 sync-cache DTOs
//! ([`ParkedRow`], [`SyncStateRow`], [`TrustRow`]).

use crate::event::content::EventType;
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::IdentityKey;
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

// ===========================================================================
// Schema v2 (IR-0201) sync-cache DTOs — all derived caches, droppable and
// re-derivable from `events` + reconnect (spec D1). The store persists and
// returns these verbatim; it does **not** re-decide validity — the sync engine
// re-validates a restored `wire` on load (spec D5).
// ===========================================================================

/// One persisted orphan-park row (`sync_parked` + its `sync_parked_missing`
/// edges) — a causally-incomplete-but-plausible frame awaiting backfill.
///
/// `wire` is the verbatim [`WireEvent`](crate::event::WireEvent) bytes (kept as
/// raw bytes, not a decoded envelope, so a corrupt/tampered row is a **logged
/// drop** the engine re-validates on load rather than a decode failure that
/// aborts the whole restore — spec D5). `author`/`park_seq`/`depth` are
/// re-derivable from `wire` + arrival but stored to keep the hot eviction path
/// off the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedRow {
    /// The parked frame's id (`BLAKE3(wire.signed)`).
    pub event_id: EventId,
    /// Verbatim `WireEvent` bytes (`== WireEvent::to_bytes()`), re-validated on load.
    pub wire: Vec<u8>,
    /// The frame's author identity (the per-author park-cap key).
    pub author: IdentityKey,
    /// Monotone arrival order, for oldest-first eviction.
    pub park_seq: u64,
    /// Backfill-chase depth this frame was parked at (bounded by config).
    pub depth: u32,
    /// The still-missing parents this frame is waiting on (drives the restored
    /// `WantEvents` retry on `open`, spec §6.3).
    pub missing: Vec<EventId>,
}

/// The single-row per-room `sync_state`: the advisory recent-chat cursor and the
/// unconfirmed higher-admin-tip suspicion that keeps a fail-closed posture across
/// a restart (spec D3 — the anti fail-open field).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStateRow {
    /// Advisory recent-chat cursor `(lamport, event_id)`; correctness never
    /// depends on it (OQ-1). `None` when unset.
    pub chat_cursor: Option<(u64, EventId)>,
    /// The unconfirmed admin tip `(id, admin_seq, attempts_remaining)` advertised
    /// by a peer but not yet backfilled. `None` = no suspicion held.
    pub suspect_tip: Option<(EventId, u64, u32)>,
}

/// One `trust_decisions` audit row (append-only). Persists a CRITICAL admin-fork
/// `equivocation` or an `admin_view_suspect` warning so a reboot cannot erase the
/// alert (PRD §13.2/§16.3, spec D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRow {
    /// Per-room monotone insertion order (assigned by the store on append).
    pub seq: u64,
    /// Stable code: `equivocation` or `admin_view_suspect`.
    pub code: String,
    /// Stable severity string: `critical` or `warning`.
    pub severity: String,
    /// The contested `admin_seq`, if any.
    pub admin_seq: Option<u64>,
    /// The implicated event ids (both branch tips for a fork).
    pub event_ids: Vec<EventId>,
    /// Advisory ms-epoch timestamp (never ordering/security).
    pub created_at: u64,
}
