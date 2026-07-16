//! The local `SQLite` event store (IR-0004).
//!
//! Persists validated events locally while keeping the append-only signed log as
//! the **single source of truth** (PRD §12 Local Storage, `PHASE-0-SPIKE.md`
//! ADR-2, Persistence note §9). Three load-bearing guarantees:
//!
//! 1. **Idempotent G-set persistence.** A [`ValidatedEvent`] is stored *exactly
//!    once* keyed by its `event_id`; a duplicate insert is ignored without error
//!    (Event Protocol §6 step 11). See [`EventStore::insert`].
//! 2. **Verbatim byte preservation.** The exact `WireEvent` bytes are stored
//!    unchanged (`events.wire`), so the store can re-broadcast and re-verify
//!    byte-for-byte.
//! 3. **Derived-cache discipline.** `event_id` + `wire` are authoritative; every
//!    other column (`room_id`, `sender_id`, `device_id`, `event_type`,
//!    `created_at`, `lamport`, `admin_seq`) and the entire `event_parents` edge
//!    table is a derived cache rebuildable from the stored events via
//!    [`EventStore::rebuild`] (spec D4) — the determinism oracle.
//!
//! The store sits **downstream of validation**: its input is a [`ValidatedEvent`]
//! from the landed [`validate_wire_bytes`](crate::event::validate_wire_bytes)
//! pipeline (#6). It does not re-decide validity, membership, authorization,
//! ordering, or sync — it provides the query surface those sibling layers consume
//! (room tail, parent lookup both directions, by-type / by-sender scans, DAG
//! heads, admin-chain tip) and the rebuild that re-folds the log.
//!
//! Out of scope here (sibling issues): the membership fold and `members` cache,
//! sync / `sync_state`, orphan buffering policy, `trust_decisions`, and CLI
//! wiring (spec §3.2). The store *persists events and records dangling parent
//! edges* so those layers can be built on a frozen substrate.

mod error;
mod model;
mod schema;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::event::cbor::{self, CborValue};
use crate::event::constants::DIGEST_LEN;
use crate::event::content::EventType;
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::IdentityKey;
use crate::event::signed::{self, SignedEvent};
use crate::event::validate::ValidatedEvent;
use crate::event::wire::WireEvent;

pub use error::StoreError;
pub use model::{InsertOutcome, InsertStats, ParkedRow, StoredEvent, SyncStateRow, TrustRow};

/// A raw 32-byte id as stored in a BLOB column.
type RawId = [u8; DIGEST_LEN];

/// The `events` columns a [`StoredEvent`] is built from, in select order.
const STORED_COLS: &str = "event_id, wire, room_id, event_type, lamport, admin_seq";

/// Connection configuration for [`EventStore::open_with`] /
/// [`EventStore::open_in_memory_with`].
///
/// `busy_timeout` controls how long a writer waits for a competing writer's
/// lock before failing with `SQLITE_BUSY` (issue #85). `Some(d)` installs a `d`
/// busy timeout; `None` opts out and fails fast on any lock collision.
///
/// Note: `rusqlite` (bundled `SQLite`) pre-installs a 5000ms `busy_timeout` on
/// every `Connection::open`/`open_in_memory`, so `None` must actively *clear*
/// that default (`sqlite3_busy_timeout(db, 0)`) rather than merely skip setting
/// one — [`EventStore::open_with`] does this unconditionally.
///
/// `#[non_exhaustive]` so future knobs are additive, but that also means a
/// crate other than this one (e.g. the SDK façade, or an embedder like
/// Bantaba) cannot build a non-default value with struct-literal syntax —
/// not even `StoreOptions { busy_timeout: Some(d), ..Default::default() }`,
/// since `non_exhaustive` rejects *any* named-field struct expression from
/// outside the defining crate. [`StoreOptions::new`] is the constructor that
/// keeps the hook actually reachable from downstream crates.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoreOptions {
    pub busy_timeout: Option<Duration>,
}

impl StoreOptions {
    /// Build options with an explicit `busy_timeout` (see the field doc for
    /// what `None` does).
    #[must_use]
    pub fn new(busy_timeout: Option<Duration>) -> Self {
        Self { busy_timeout }
    }
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            busy_timeout: Some(Duration::from_secs(5)),
        }
    }
}

/// A synchronous, `rusqlite`-backed local event store.
///
/// Wraps a single [`Connection`]. Write transactions use `BEGIN IMMEDIATE`
/// (rather than the rusqlite default `BEGIN DEFERRED`), so a colliding writer
/// waits (bounded by the connection's `busy_timeout`, see [`StoreOptions`])
/// instead of failing with an un-retryable lock-upgrade `SQLITE_BUSY`. Reads use
/// the connection's prepared-statement cache. Not `Sync`; share across threads
/// behind your own `Mutex` if needed — opening multiple `EventStore` connections
/// onto the same file is the supported way to get concurrent writers today
/// (spec §10, multi-connection pooling is future work).
pub struct EventStore {
    conn: Connection,
    /// Test-only deterministic fault injection: the number of upcoming
    /// [`insert`](Self::insert) calls that fail with an injected error before
    /// touching the database (issue #119 — the engine's insert-failure recovery
    /// is untestable against real `SQLite` without nondeterministic disk-full
    /// tricks). Compiled out of non-test builds.
    #[cfg(test)]
    fail_next_inserts: u32,
}

impl EventStore {
    /// Open (creating if absent) a store at `path` with the default
    /// [`StoreOptions`] (a 5000ms `busy_timeout`), applying pragmas and the
    /// idempotent schema migration.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] if the file cannot be opened, or
    /// [`StoreError::Migration`] if it carries a newer unknown schema version.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, &StoreOptions::default())
    }

    /// Open (creating if absent) a store at `path` with explicit [`StoreOptions`].
    ///
    /// # Errors
    /// As [`EventStore::open`].
    pub fn open_with(path: &Path, opts: &StoreOptions) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_connection_with(conn, opts)
    }

    /// Open a private in-memory store (tests / ephemeral derivations) with the
    /// default [`StoreOptions`].
    ///
    /// # Errors
    /// As [`EventStore::open`].
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open_in_memory_with(&StoreOptions::default())
    }

    /// Open a private in-memory store with explicit [`StoreOptions`].
    ///
    /// # Errors
    /// As [`EventStore::open`].
    pub fn open_in_memory_with(opts: &StoreOptions) -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection_with(conn, opts)
    }

    fn from_connection_with(conn: Connection, opts: &StoreOptions) -> Result<Self, StoreError> {
        schema::apply_pragmas(&conn)?;
        // Unconditional: rusqlite pre-installs a 5000ms busy_timeout on open, so a
        // conditional `if let Some(d) = ...` would leave that default in place and
        // make `busy_timeout: None` a silent no-op instead of a real fail-fast
        // opt-out. `Duration::ZERO` clears the handler (`sqlite3_busy_timeout(db, 0)`).
        conn.busy_timeout(opts.busy_timeout.unwrap_or(Duration::ZERO))?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn,
            #[cfg(test)]
            fail_next_inserts: 0,
        })
    }

    /// Test-only: make the next `n` [`insert`](Self::insert) calls fail with an
    /// injected [`StoreError`] without touching the database — the deterministic
    /// stand-in for a disk-full / I/O fault (issue #119).
    #[cfg(test)]
    pub(crate) fn fail_next_inserts(&mut self, n: u32) {
        self.fail_next_inserts = n;
    }

    /// Begin a write transaction that grabs the write lock up front
    /// (`BEGIN IMMEDIATE`), so a colliding writer *waits* (bounded by the
    /// connection's `busy_timeout`) instead of failing with `SQLITE_BUSY`, and
    /// read-then-write bodies (e.g. [`EventStore::append_trust_decision`]) never
    /// hit the un-retryable lock-upgrade deadlock a `BEGIN DEFERRED` transaction
    /// can hit under concurrent writers (issue #85).
    fn begin_write(&mut self) -> Result<rusqlite::Transaction<'_>, StoreError> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    // -- write path -----------------------------------------------------------

    /// Idempotently persist a pre-validated event (Event Protocol §6 step 11).
    ///
    /// Returns [`InsertOutcome::Duplicate`] (a no-op) if `event_id` is already
    /// stored — never an error. On insert, derived `lamport`/`admin_seq` are
    /// computed eagerly when resolvable, and any already-stored descendants whose
    /// missing parent just arrived are recomputed (spec §6 policy).
    ///
    /// # Errors
    /// [`StoreError::Integrity`] if `BLAKE3(wire.signed) != ev.event_id` (a caller
    /// passed a mismatched id/bytes pair); [`StoreError::Sqlite`] on a DB error.
    pub fn insert(&mut self, ev: &ValidatedEvent) -> Result<InsertOutcome, StoreError> {
        #[cfg(test)]
        if self.fail_next_inserts > 0 {
            self.fail_next_inserts -= 1;
            return Err(StoreError::integrity("injected insert fault (test)"));
        }
        let tx = self.begin_write()?;
        let outcome = insert_in_tx(&tx, ev)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Persist many events in a single transaction; returns the
    /// inserted/duplicate counts.
    ///
    /// # Errors
    /// As [`EventStore::insert`]; the whole batch rolls back on the first error.
    pub fn insert_all(&mut self, evs: &[ValidatedEvent]) -> Result<InsertStats, StoreError> {
        let tx = self.begin_write()?;
        let mut stats = InsertStats::default();
        for ev in evs {
            stats.record(insert_in_tx(&tx, ev)?);
        }
        tx.commit()?;
        Ok(stats)
    }

    // -- point / existence ----------------------------------------------------

    /// Whether an event with this id is stored.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn contains(&self, id: &EventId) -> Result<bool, StoreError> {
        let found = self
            .conn
            .prepare_cached("SELECT 1 FROM events WHERE event_id = ?1")?
            .query_row(params![&id.as_bytes()[..]], |_| Ok(()))
            .optional()?;
        Ok(found.is_some())
    }

    /// Whether an event id is stored in the specified room.
    ///
    /// Sync engines share one database across rooms, so an event id is never a
    /// capability by itself. Network responders must use this room-scoped form
    /// before treating an id as locally held.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn contains_in_room(&self, room: &RoomId, id: &EventId) -> Result<bool, StoreError> {
        let found = self
            .conn
            .prepare_cached("SELECT 1 FROM events WHERE room_id = ?1 AND event_id = ?2")?
            .query_row(
                params![&room.as_bytes()[..], &id.as_bytes()[..]],
                |_| Ok(()),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Fetch a single stored event by id.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Decode`] /
    /// [`StoreError::Integrity`] if the stored bytes are corrupt.
    pub fn get(&self, id: &EventId) -> Result<Option<StoredEvent>, StoreError> {
        let sql = format!("SELECT {STORED_COLS} FROM events WHERE event_id = ?1");
        let mut found = stored_query(&self.conn, &sql, params![&id.as_bytes()[..]])?;
        Ok(found.pop())
    }

    /// Fetch one event only when both its id and room match.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Decode`] /
    /// [`StoreError::Integrity`] if the stored bytes are corrupt.
    pub fn get_in_room(
        &self,
        room: &RoomId,
        id: &EventId,
    ) -> Result<Option<StoredEvent>, StoreError> {
        let sql = format!("SELECT {STORED_COLS} FROM events WHERE room_id = ?1 AND event_id = ?2");
        let mut found = stored_query(
            &self.conn,
            &sql,
            params![&room.as_bytes()[..], &id.as_bytes()[..]],
        )?;
        Ok(found.pop())
    }

    /// Count stored events in a room.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn count(&self, room: &RoomId) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .prepare_cached("SELECT COUNT(*) FROM events WHERE room_id = ?1")?
            .query_row(params![&room.as_bytes()[..]], |row| row.get(0))?;
        u64::try_from(n).map_err(|_| StoreError::integrity("negative row count"))
    }

    /// Every stored `event_id` in a room, as a deterministic [`BTreeSet`] — the
    /// read-only set-equality oracle the sync layer's convergence assertion is
    /// built on (spec `bounded-recent-sync-prototype.md` D8).
    ///
    /// Returns **all** stored events regardless of causal completeness; the store
    /// holds exactly the fold-accepted set (sync spec D5), so this is precisely the
    /// convergent validated set two peers compare after a sync round. Additive and
    /// read-only — no schema or `user_version` change.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if a
    /// stored id is not 32 bytes.
    pub fn room_event_ids(
        &self,
        room: &RoomId,
    ) -> Result<std::collections::BTreeSet<EventId>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT event_id FROM events WHERE room_id = ?1")?;
        let rows = stmt.query_map(params![&room.as_bytes()[..]], |row| {
            row.get::<_, Vec<u8>>(0)
        })?;
        let mut out = std::collections::BTreeSet::new();
        for r in rows {
            out.insert(EventId::from_bytes(to_raw_id(&r?)?));
        }
        Ok(out)
    }

    /// Every distinct `room_id` present in the store, ascending by raw id
    /// (bytewise `memcmp`, which is exactly [`RoomId`]'s `Ord`).
    ///
    /// The substrate for a global "which room owns this `pipe_id`?" scan
    /// (`iroh-rooms pipe close <PIPE_ID>` room inference, spec IR-0108 §4.2) and a
    /// future `room ls`. Additive and read-only — no schema or `user_version`
    /// change (mirrors [`EventStore::room_event_ids`]).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if a
    /// stored `room_id` is not 32 bytes.
    pub fn room_ids(&self) -> Result<Vec<RoomId>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT room_id FROM events ORDER BY room_id")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(RoomId::from_bytes(to_raw_id(&r?)?));
        }
        Ok(out)
    }

    // -- parent lookup (both directions) --------------------------------------

    /// The declared `prev_events` of an event, in signed order (`ordinal`).
    /// Some entries may be **dangling** (not yet stored) — see
    /// [`EventStore::missing_parents`].
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn parents_of(&self, id: &EventId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT parent_id FROM event_parents WHERE child_id = ?1 ORDER BY ordinal",
            params![&id.as_bytes()[..]],
        )
    }

    /// The stored events that cite `id` as a parent (the reverse edge), ordered
    /// by `event_id`. This is what lets the fold/sync re-process buffered
    /// children when a parent arrives.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn children_of(&self, id: &EventId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT child_id FROM event_parents WHERE parent_id = ?1 ORDER BY child_id",
            params![&id.as_bytes()[..]],
        )
    }

    /// The `prev_events` of `id` that are **not** yet stored (dangling refs,
    /// out-of-order arrival §4), in signed order.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn missing_parents(&self, id: &EventId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT parent_id FROM event_parents \
             WHERE child_id = ?1 AND parent_id NOT IN (SELECT event_id FROM events) \
             ORDER BY ordinal",
            params![&id.as_bytes()[..]],
        )
    }

    /// The parents of `id` that are not stored in `room`.
    ///
    /// A parent with the same id in a different room must remain missing for
    /// this room's sync engine. Otherwise a shared database can make a foreign
    /// event satisfy a local causal dependency.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn missing_parents_in_room(
        &self,
        room: &RoomId,
        id: &EventId,
    ) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT p.parent_id FROM event_parents p \
             WHERE p.child_id = ?1 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM events e \
                   WHERE e.room_id = ?2 AND e.event_id = p.parent_id \
               ) \
             ORDER BY p.ordinal",
            params![&id.as_bytes()[..], &room.as_bytes()[..]],
        )
    }

    // -- room tail / canonical order ------------------------------------------

    /// The most-recent `limit` causally-placed events in a room, in ascending
    /// canonical `(lamport, event_id)` order.
    ///
    /// Events with a `NULL` lamport (not yet causally complete) are excluded
    /// (Membership §2.3). `event_id` is compared bytewise (raw BLOB `memcmp`),
    /// which is exactly the §2.1 tie-break.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or a decode/integrity error on
    /// corrupt stored bytes.
    pub fn room_tail(&self, room: &RoomId, limit: u32) -> Result<Vec<StoredEvent>, StoreError> {
        let sql = format!(
            "SELECT {STORED_COLS} FROM events \
             WHERE room_id = ?1 AND lamport IS NOT NULL \
             ORDER BY lamport DESC, event_id DESC LIMIT ?2"
        );
        let mut rows = stored_query(
            &self.conn,
            &sql,
            params![&room.as_bytes()[..], i64::from(limit)],
        )?;
        rows.reverse(); // DESC fetch → ascending canonical order for display
        Ok(rows)
    }

    // -- membership-fold inputs -----------------------------------------------

    /// All events of a given type in a room (membership-fold input). Ordered by
    /// `(lamport, event_id)` with causally-incomplete (`NULL` lamport) events
    /// last.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or a decode/integrity error on
    /// corrupt stored bytes.
    pub fn by_type(&self, room: &RoomId, ty: EventType) -> Result<Vec<StoredEvent>, StoreError> {
        let sql = format!(
            "SELECT {STORED_COLS} FROM events \
             WHERE room_id = ?1 AND event_type = ?2 \
             ORDER BY lamport IS NULL, lamport, event_id"
        );
        stored_query(&self.conn, &sql, params![&room.as_bytes()[..], ty.as_str()])
    }

    /// All events authored by a sender in a room (membership-fold input). Ordered
    /// like [`EventStore::by_type`].
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or a decode/integrity error on
    /// corrupt stored bytes.
    pub fn by_sender(
        &self,
        room: &RoomId,
        sender: &IdentityKey,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let sql = format!(
            "SELECT {STORED_COLS} FROM events \
             WHERE room_id = ?1 AND sender_id = ?2 \
             ORDER BY lamport IS NULL, lamport, event_id"
        );
        stored_query(
            &self.conn,
            &sql,
            params![&room.as_bytes()[..], &sender.as_bytes()[..]],
        )
    }

    /// The DAG heads of a room: stored events that no stored **same-room** event
    /// cites as a parent (Membership §3.4 causal heads), ordered by `event_id`.
    ///
    /// The citing child is required to live in the same room (mirroring
    /// [`missing_parents_in_room`](Self::missing_parents_in_room)): in a shared
    /// database a foreign room's edge row must not un-head a local event. Ids
    /// are content hashes over bytes that include the `room_id`, so a cross-room
    /// citation cannot occur without a hash collision — the scoping is defensive
    /// hardening now that heads feed the `WantMembership` `have` claim (#113).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn heads(&self, room: &RoomId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT e.event_id FROM events e \
             WHERE e.room_id = ?1 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM event_parents p \
                   JOIN events c ON c.event_id = p.child_id \
                   WHERE p.parent_id = e.event_id AND c.room_id = ?1 \
               ) \
             ORDER BY e.event_id",
            params![&room.as_bytes()[..]],
        )
    }

    /// A `limit`-sized page of the room's **causally-placed** event ids, in
    /// descending canonical `(lamport, event_id)` order, starting `offset` rows
    /// from the newest — the recent-lamport slab and the rotating claim window
    /// of the `WantMembership` `have` claim (#113).
    ///
    /// Only rows with a non-`NULL` `lamport` qualify. A non-`NULL` lamport is
    /// derived as `1 + max(parent lamports)` and poisoned by any absent or
    /// unplaced parent, so — inductively — every id returned here is held **with
    /// its complete ancestry**, which is exactly what an ancestry claim asserts.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn recent_event_ids(
        &self,
        room: &RoomId,
        limit: u32,
        offset: u64,
    ) -> Result<Vec<EventId>, StoreError> {
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        id_query(
            &self.conn,
            "SELECT event_id FROM events \
             WHERE room_id = ?1 AND lamport IS NOT NULL \
             ORDER BY lamport DESC, event_id DESC \
             LIMIT ?2 OFFSET ?3",
            params![&room.as_bytes()[..], i64::from(limit), offset],
        )
    }

    /// The room's **causally-placed** DAG heads: [`heads`](Self::heads) restricted
    /// to rows with a derived lamport, ordered by `event_id`. These are the only
    /// heads a `WantMembership` ancestry claim may cite (#113): a `NULL`-lamport
    /// head sits above a local hole and cannot back an ancestry claim.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn placed_heads(&self, room: &RoomId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT e.event_id FROM events e \
             WHERE e.room_id = ?1 AND e.lamport IS NOT NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM event_parents p \
                   JOIN events c ON c.event_id = p.child_id \
                   WHERE p.parent_id = e.event_id AND c.room_id = ?1 \
               ) \
             ORDER BY e.event_id",
            params![&room.as_bytes()[..]],
        )
    }

    /// How many **causally-placed** (non-`NULL` lamport) events the room holds —
    /// the span the `WantMembership` claim's rotating window sweeps (#113).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] on a
    /// negative count.
    pub fn placed_count(&self, room: &RoomId) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .prepare_cached(
                "SELECT COUNT(*) FROM events WHERE room_id = ?1 AND lamport IS NOT NULL",
            )?
            .query_row(params![&room.as_bytes()[..]], |row| row.get(0))?;
        u64::try_from(n).map_err(|_| StoreError::integrity("negative placed count"))
    }

    // -- admin tip ------------------------------------------------------------

    /// The admin-chain tip of a room: the admin-authored event with the highest
    /// derived `admin_seq` (Membership §0), tie-broken by lowest `event_id`.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if a
    /// stored `admin_seq` is negative.
    pub fn admin_chain_tip(&self, room: &RoomId) -> Result<Option<(EventId, u64)>, StoreError> {
        let row = self
            .conn
            .prepare_cached(
                "SELECT event_id, admin_seq FROM events \
                 WHERE room_id = ?1 AND admin_seq IS NOT NULL \
                 ORDER BY admin_seq DESC, event_id ASC LIMIT 1",
            )?
            .query_row(params![&room.as_bytes()[..]], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?;
        match row {
            Some((id, seq)) => {
                let id = EventId::from_bytes(to_raw_id(&id)?);
                Ok(Some((id, sql_to_u64(seq)?)))
            }
            None => Ok(None),
        }
    }

    // -- derived-cache maintenance --------------------------------------------

    /// Clear **all** derived state and recompute it purely from the authoritative
    /// `(event_id, wire)` rows (spec D4) — the proof that derived caches are
    /// rebuildable from stored events / restart determinism.
    ///
    /// Re-decodes each `wire` with the landed strict reader, integrity-asserts
    /// `BLAKE3(wire.signed) == event_id`, repopulates the derived columns and
    /// `event_parents`, then recomputes `lamport`/`admin_seq` with an
    /// order-independent topological (least-fixpoint) pass.
    ///
    /// # Errors
    /// [`StoreError::Decode`] if a stored `wire` fails to decode (corruption),
    /// [`StoreError::Integrity`] if a recomputed id disagrees with its key, or
    /// [`StoreError::Sqlite`] on a DB error. Never panics on stored bytes.
    pub fn rebuild(&mut self) -> Result<(), StoreError> {
        let tx = self.begin_write()?;
        rebuild_in_tx(&tx)?;
        tx.commit()?;
        Ok(())
    }

    // -- schema-v2 sync-cache (IR-0201) ---------------------------------------
    //
    // All five tables below are DERIVED CACHES (spec D1): droppable and
    // re-derivable from `events` + reconnect. The store persists and returns them
    // verbatim and re-decides no validity — the sync engine re-validates a
    // restored `wire` on load (spec D5). Writes go through short transactions; a
    // checkpoint miss degrades durability, never correctness (`events` stays
    // authoritative), so callers treat a failure as non-fatal (spec §6.2).

    /// Load the per-room `sync_state` row (advisory cursor + unconfirmed
    /// admin-tip suspicion), or `None` if none was ever written.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if a
    /// stored id/counter is malformed.
    pub fn load_sync_state(&self, room: &RoomId) -> Result<Option<SyncStateRow>, StoreError> {
        let row = self
            .conn
            .prepare_cached(
                "SELECT chat_cursor_lamport, chat_cursor_event, \
                        suspect_tip_event, suspect_tip_seq, suspect_tip_attempts \
                 FROM sync_state WHERE room_id = ?1",
            )?
            .query_row(params![&room.as_bytes()[..]], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .optional()?;
        let Some((cur_l, cur_e, susp_e, susp_s, susp_a)) = row else {
            return Ok(None);
        };
        let chat_cursor = match (cur_l, cur_e) {
            (Some(l), Some(e)) => Some((sql_to_u64(l)?, EventId::from_bytes(to_raw_id(&e)?))),
            _ => None,
        };
        let suspect_tip = match (susp_e, susp_s) {
            (Some(e), Some(s)) => Some((
                EventId::from_bytes(to_raw_id(&e)?),
                sql_to_u64(s)?,
                u32::try_from(susp_a)
                    .map_err(|_| StoreError::integrity("suspect_tip_attempts out of range"))?,
            )),
            _ => None,
        };
        Ok(Some(SyncStateRow {
            chat_cursor,
            suspect_tip,
        }))
    }

    /// Upsert the single per-room `sync_state` row (`updated_at` is advisory and
    /// stored as `0`, keeping the write clock-free and restart-deterministic).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if a
    /// counter exceeds the `SQLite` INTEGER range.
    pub fn save_sync_state(&mut self, room: &RoomId, st: &SyncStateRow) -> Result<(), StoreError> {
        let (cur_l, cur_e) = match &st.chat_cursor {
            Some((l, e)) => (Some(u64_to_sql(*l)?), Some(e.as_bytes().to_vec())),
            None => (None, None),
        };
        let (susp_e, susp_s, susp_a) = match &st.suspect_tip {
            Some((id, seq, att)) => (
                Some(id.as_bytes().to_vec()),
                Some(u64_to_sql(*seq)?),
                i64::from(*att),
            ),
            None => (None, None, 0_i64),
        };
        let tx = self.begin_write()?;
        tx.prepare_cached(
            "INSERT INTO sync_state \
                (room_id, chat_cursor_lamport, chat_cursor_event, \
                 suspect_tip_event, suspect_tip_seq, suspect_tip_attempts, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0) \
             ON CONFLICT(room_id) DO UPDATE SET \
                chat_cursor_lamport = excluded.chat_cursor_lamport, \
                chat_cursor_event   = excluded.chat_cursor_event, \
                suspect_tip_event    = excluded.suspect_tip_event, \
                suspect_tip_seq      = excluded.suspect_tip_seq, \
                suspect_tip_attempts = excluded.suspect_tip_attempts, \
                updated_at           = excluded.updated_at",
        )?
        .execute(params![
            &room.as_bytes()[..],
            cur_l,
            cur_e,
            susp_e,
            susp_s,
            susp_a
        ])?;
        tx.commit()?;
        Ok(())
    }

    /// Load the per-author backfill token buckets for a room (empty if none).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] on a
    /// malformed author id / negative token count.
    pub fn load_backfill_tokens(
        &self,
        room: &RoomId,
    ) -> Result<BTreeMap<IdentityKey, u32>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT author_id, tokens FROM sync_backfill_tokens WHERE room_id = ?1",
        )?;
        let rows = stmt.query_map(params![&room.as_bytes()[..]], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut out = BTreeMap::new();
        for r in rows {
            let (author, tokens) = r?;
            let author = IdentityKey::from_bytes(to_raw_id(&author)?);
            let tokens = u32::try_from(tokens)
                .map_err(|_| StoreError::integrity("backfill token count out of range"))?;
            out.insert(author, tokens);
        }
        Ok(out)
    }

    /// Replace the room's backfill token buckets with `tokens`, in one
    /// transaction (the batched per-tick checkpoint, spec §6.2 / D4).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn save_backfill_tokens(
        &mut self,
        room: &RoomId,
        tokens: &BTreeMap<IdentityKey, u32>,
    ) -> Result<(), StoreError> {
        let tx = self.begin_write()?;
        tx.execute(
            "DELETE FROM sync_backfill_tokens WHERE room_id = ?1",
            params![&room.as_bytes()[..]],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO sync_backfill_tokens (room_id, author_id, tokens) VALUES (?1, ?2, ?3)",
            )?;
            for (author, t) in tokens {
                stmt.execute(params![
                    &room.as_bytes()[..],
                    &author.as_bytes()[..],
                    i64::from(*t)
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Load the persisted orphan park for a room, oldest-first by `park_seq` (the
    /// deterministic re-insertion order), each row carrying its still-missing
    /// parents. The `wire` bytes are returned verbatim and re-validated by the
    /// engine on load (spec D5) — this method does **not** decode them.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] on a
    /// malformed stored id / out-of-range counter.
    pub fn load_parked(&self, room: &RoomId) -> Result<Vec<ParkedRow>, StoreError> {
        // (event_id, wire, author_id, park_seq, depth) — the raw `sync_parked` row
        // before fallible id/counter conversion.
        let raw: Vec<RawParkedRow> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT event_id, wire, author_id, park_seq, depth FROM sync_parked \
                 WHERE room_id = ?1 ORDER BY park_seq, event_id",
            )?;
            let rows = stmt.query_map(params![&room.as_bytes()[..]], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        let mut out = Vec::with_capacity(raw.len());
        for (event_id, wire, author, park_seq, depth) in raw {
            let event_id = EventId::from_bytes(to_raw_id(&event_id)?);
            let author = IdentityKey::from_bytes(to_raw_id(&author)?);
            let park_seq = sql_to_u64(park_seq)?;
            let depth = u32::try_from(depth)
                .map_err(|_| StoreError::integrity("parked depth out of range"))?;
            let missing = self.parked_missing(room, &event_id)?;
            out.push(ParkedRow {
                event_id,
                wire,
                author,
                park_seq,
                depth,
                missing,
            });
        }
        Ok(out)
    }

    /// The persisted still-missing parents of one parked frame.
    fn parked_missing(&self, room: &RoomId, id: &EventId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT missing_id FROM sync_parked_missing \
             WHERE room_id = ?1 AND event_id = ?2 ORDER BY missing_id",
            params![&room.as_bytes()[..], &id.as_bytes()[..]],
        )
    }

    /// Insert (or refresh) one parked frame and its missing-parent edge set, in
    /// one transaction. Idempotent on `(room_id, event_id)` (checkpoint replay is
    /// a no-op, spec §8.3).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if
    /// `park_seq` exceeds the `SQLite` INTEGER range.
    pub fn upsert_parked(&mut self, room: &RoomId, row: &ParkedRow) -> Result<(), StoreError> {
        let park_seq = u64_to_sql(row.park_seq)?;
        let tx = self.begin_write()?;
        tx.prepare_cached(
            "INSERT INTO sync_parked (room_id, event_id, wire, author_id, park_seq, depth) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(room_id, event_id) DO UPDATE SET \
                wire = excluded.wire, author_id = excluded.author_id, \
                park_seq = excluded.park_seq, depth = excluded.depth",
        )?
        .execute(params![
            &room.as_bytes()[..],
            &row.event_id.as_bytes()[..],
            &row.wire,
            &row.author.as_bytes()[..],
            park_seq,
            i64::from(row.depth),
        ])?;
        // Replace the missing-parent edge set for this frame.
        tx.execute(
            "DELETE FROM sync_parked_missing WHERE room_id = ?1 AND event_id = ?2",
            params![&room.as_bytes()[..], &row.event_id.as_bytes()[..]],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO sync_parked_missing (room_id, event_id, missing_id) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for m in &row.missing {
                stmt.execute(params![
                    &room.as_bytes()[..],
                    &row.event_id.as_bytes()[..],
                    &m.as_bytes()[..]
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete one parked frame (its `sync_parked_missing` edges cascade via the
    /// foreign key). A no-op if the frame is not present.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn delete_parked(&mut self, room: &RoomId, id: &EventId) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM sync_parked WHERE room_id = ?1 AND event_id = ?2",
            params![&room.as_bytes()[..], &id.as_bytes()[..]],
        )?;
        Ok(())
    }

    /// Load the append-only trust-decision audit trail for a room, in insertion
    /// order (`seq`).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] on a
    /// malformed stored row (bad `event_ids` CBOR / out-of-range counter).
    pub fn load_trust_decisions(&self, room: &RoomId) -> Result<Vec<TrustRow>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT seq, code, severity, admin_seq, event_ids, created_at \
             FROM trust_decisions WHERE room_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![&room.as_bytes()[..]], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (seq, code, severity, admin_seq, event_ids, created_at) = r?;
            out.push(TrustRow {
                seq: sql_to_u64(seq)?,
                code,
                severity,
                admin_seq: admin_seq.map(sql_to_u64).transpose()?,
                event_ids: decode_id_array(&event_ids)?,
                created_at: sql_to_u64(created_at)?,
            });
        }
        Ok(out)
    }

    /// Append one trust decision, assigning the next per-room monotone `seq`
    /// (returned). Append-only: never overwrites a prior alert (spec D6).
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error, or [`StoreError::Integrity`] if a
    /// counter exceeds the `SQLite` INTEGER range.
    pub fn append_trust_decision(
        &mut self,
        room: &RoomId,
        row: &TrustRow,
    ) -> Result<u64, StoreError> {
        let admin_seq = row.admin_seq.map(u64_to_sql).transpose()?;
        let created_at = u64_to_sql(row.created_at)?;
        let event_ids = encode_id_array(&row.event_ids);
        let tx = self.begin_write()?;
        let next: i64 = tx
            .prepare_cached(
                "SELECT COALESCE(MAX(seq) + 1, 0) FROM trust_decisions WHERE room_id = ?1",
            )?
            .query_row(params![&room.as_bytes()[..]], |r| r.get(0))?;
        tx.prepare_cached(
            "INSERT INTO trust_decisions \
                (room_id, seq, code, severity, admin_seq, event_ids, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            &room.as_bytes()[..],
            next,
            row.code.as_str(),
            row.severity.as_str(),
            admin_seq,
            event_ids,
            created_at,
        ])?;
        tx.commit()?;
        sql_to_u64(next)
    }
}

// ---------------------------------------------------------------------------
// Write path (free fns operating on a transaction / connection)
// ---------------------------------------------------------------------------

/// Insert one event within an open transaction, returning the outcome.
fn insert_in_tx(tx: &Connection, ev: &ValidatedEvent) -> Result<InsertOutcome, StoreError> {
    // Integrity guard (spec D5): the cheap re-derivation must match the key.
    let recomputed = signed::event_id_from_bytes(&ev.wire.signed);
    if recomputed != ev.event_id {
        return Err(StoreError::integrity(
            "event_id does not match BLAKE3(wire.signed)",
        ));
    }

    let event_id: &[u8] = ev.event_id.as_bytes();
    let wire_bytes = ev.wire.to_bytes();

    let inserted = tx
        .prepare_cached(
            "INSERT INTO events \
             (event_id, wire, room_id, sender_id, device_id, event_type, created_at, lamport, admin_seq) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL) \
             ON CONFLICT(event_id) DO NOTHING",
        )?
        .execute(params![
            event_id,
            wire_bytes,
            &ev.event.room_id.as_bytes()[..],
            &ev.event.sender_id.as_bytes()[..],
            &ev.event.device_id.as_bytes()[..],
            ev.event.event_type.as_str(),
            created_at_to_sql(ev.event.created_at),
        ])?;

    if inserted == 0 {
        return Ok(InsertOutcome::Duplicate);
    }

    // Record parent edges (parent_id may dangle, §4 / D6).
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO event_parents (child_id, parent_id, ordinal) VALUES (?1, ?2, ?3)",
        )?;
        for (i, parent) in ev.event.prev_events.iter().enumerate() {
            let ordinal = i64::try_from(i)
                .map_err(|_| StoreError::integrity("prev_events ordinal overflow"))?;
            stmt.execute(params![event_id, &parent.as_bytes()[..], ordinal])?;
        }
    }

    // Compute this event's derived values, then resolve any now-reachable
    // descendants whose missing parent just arrived.
    propagate_from(tx, event_id)?;
    Ok(InsertOutcome::Inserted)
}

/// Recompute `lamport`/`admin_seq` for `start` and forward-propagate to children
/// whenever a value changes (least-fixpoint over the present graph).
fn propagate_from(tx: &Connection, start: &[u8]) -> Result<(), StoreError> {
    let mut stack: Vec<RawId> = vec![to_raw_id(start)?];
    while let Some(id) = stack.pop() {
        if recompute_one(tx, &id)? {
            for child in raw_children_of(tx, &id)? {
                stack.push(child);
            }
        }
    }
    Ok(())
}

/// Recompute one event's derived values from current DB state; returns whether
/// they changed. A no-op (returns `false`) if no row exists for `id`.
fn recompute_one(tx: &Connection, id: &RawId) -> Result<bool, StoreError> {
    let row = tx
        .prepare_cached(
            "SELECT room_id, sender_id, event_type, lamport, admin_seq FROM events WHERE event_id = ?1",
        )?
        .query_row(params![&id[..]], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .optional()?;
    let Some((room_id, sender_id, event_type, cur_lamport, cur_admin)) = row else {
        return Ok(false);
    };

    let parents = raw_parents_of(tx, id)?;
    let is_genesis = event_type == EventType::RoomCreated.as_str();

    // lamport: genesis ⇒ 0; else 1 + max(parent lamports) iff every parent is
    // present with a known lamport, else NULL.
    let new_lamport = if is_genesis && parents.is_empty() {
        Some(0)
    } else if parents.is_empty() {
        None
    } else {
        let mut max = -1_i64;
        let mut all_known = true;
        for p in &parents {
            if let Some(l) = lamport_of(tx, p)? {
                max = max.max(l);
            } else {
                all_known = false;
                break;
            }
        }
        all_known.then_some(max + 1)
    };

    // admin_seq: only for admin-authored events once the genesis (admin identity)
    // is present; genesis ⇒ 0; else 1 + max(defined parent admin_seqs).
    let new_admin = match admin_of_room(tx, &room_id)? {
        Some(admin) if admin[..] == sender_id[..] => {
            if is_genesis {
                Some(0)
            } else {
                let mut max: Option<i64> = None;
                for p in &parents {
                    if let Some(a) = admin_seq_of(tx, p)? {
                        max = Some(max.map_or(a, |m| m.max(a)));
                    }
                }
                max.map(|m| m + 1)
            }
        }
        _ => None,
    };

    if new_lamport == cur_lamport && new_admin == cur_admin {
        return Ok(false);
    }
    tx.prepare_cached("UPDATE events SET lamport = ?2, admin_seq = ?3 WHERE event_id = ?1")?
        .execute(params![&id[..], new_lamport, new_admin])?;
    Ok(true)
}

/// The `sender_id` of the room's genesis (`room.created`), the MVP admin
/// identity, or `None` if the genesis is not stored yet.
fn admin_of_room(tx: &Connection, room_id: &[u8]) -> Result<Option<RawId>, StoreError> {
    let sender = tx
        .prepare_cached(
            "SELECT sender_id FROM events \
             WHERE room_id = ?1 AND event_type = ?2 ORDER BY event_id LIMIT 1",
        )?
        .query_row(params![room_id, EventType::RoomCreated.as_str()], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()?;
    sender.map(|s| to_raw_id(&s)).transpose()
}

/// The stored `lamport` of an event, or `None` if absent (dangling) or unknown.
fn lamport_of(tx: &Connection, id: &RawId) -> Result<Option<i64>, StoreError> {
    let v = tx
        .prepare_cached("SELECT lamport FROM events WHERE event_id = ?1")?
        .query_row(params![&id[..]], |row| row.get::<_, Option<i64>>(0))
        .optional()?;
    Ok(v.flatten())
}

/// The stored `admin_seq` of an event, or `None` if absent or undefined.
fn admin_seq_of(tx: &Connection, id: &RawId) -> Result<Option<i64>, StoreError> {
    let v = tx
        .prepare_cached("SELECT admin_seq FROM events WHERE event_id = ?1")?
        .query_row(params![&id[..]], |row| row.get::<_, Option<i64>>(0))
        .optional()?;
    Ok(v.flatten())
}

/// Raw parent ids of `id` in `ordinal` order.
fn raw_parents_of(tx: &Connection, id: &RawId) -> Result<Vec<RawId>, StoreError> {
    raw_id_query(
        tx,
        "SELECT parent_id FROM event_parents WHERE child_id = ?1 ORDER BY ordinal",
        params![&id[..]],
    )
}

/// Raw child ids that cite `id` as a parent.
fn raw_children_of(tx: &Connection, id: &RawId) -> Result<Vec<RawId>, StoreError> {
    raw_id_query(
        tx,
        "SELECT child_id FROM event_parents WHERE parent_id = ?1 ORDER BY child_id",
        params![&id[..]],
    )
}

// ---------------------------------------------------------------------------
// Rebuild (D4)
// ---------------------------------------------------------------------------

/// Recompute the entire derived cache from the authoritative `(event_id, wire)`
/// projection, within an open transaction.
fn rebuild_in_tx(tx: &Connection) -> Result<(), StoreError> {
    // 1. Snapshot the authoritative projection.
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = {
        let mut stmt = tx.prepare("SELECT event_id, wire FROM events")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        v
    };

    // 2. Clear all derived state (edges; columns reset per-row below).
    tx.execute("DELETE FROM event_parents", [])?;

    // 3. Re-derive structural columns + edges from `wire`; integrity-check keys.
    for (event_id, wire_bytes) in &pairs {
        rebuild_row(tx, event_id, wire_bytes)?;
    }

    // 4. Order-independent topological lamport/admin_seq pass.
    recompute_all(tx)?;
    Ok(())
}

/// Re-derive one row's structural columns and parent edges from its stored wire.
fn rebuild_row(tx: &Connection, event_id: &[u8], wire_bytes: &[u8]) -> Result<(), StoreError> {
    let wire = WireEvent::decode(wire_bytes).map_err(StoreError::Decode)?;
    let recomputed = signed::event_id_from_bytes(&wire.signed);
    if recomputed.as_bytes()[..] != *event_id {
        return Err(StoreError::integrity(
            "rebuild: BLAKE3(wire.signed) != stored event_id",
        ));
    }
    let event = SignedEvent::decode(&wire.signed).map_err(StoreError::Decode)?;

    tx.prepare_cached(
        "UPDATE events SET \
            room_id = ?2, sender_id = ?3, device_id = ?4, event_type = ?5, created_at = ?6, \
            lamport = NULL, admin_seq = NULL \
         WHERE event_id = ?1",
    )?
    .execute(params![
        event_id,
        &event.room_id.as_bytes()[..],
        &event.sender_id.as_bytes()[..],
        &event.device_id.as_bytes()[..],
        event.event_type.as_str(),
        created_at_to_sql(event.created_at),
    ])?;

    let mut stmt = tx.prepare_cached(
        "INSERT OR IGNORE INTO event_parents (child_id, parent_id, ordinal) VALUES (?1, ?2, ?3)",
    )?;
    for (i, parent) in event.prev_events.iter().enumerate() {
        let ordinal =
            i64::try_from(i).map_err(|_| StoreError::integrity("prev_events ordinal overflow"))?;
        stmt.execute(params![event_id, &parent.as_bytes()[..], ordinal])?;
    }
    Ok(())
}

/// Static per-event facts the fixpoint reads (no derived values).
struct NodeStatic {
    room: RawId,
    sender: RawId,
    is_genesis: bool,
    parents: Vec<RawId>,
}

/// Recompute every event's `lamport`/`admin_seq` as the least fixpoint of the
/// derivation equations (order-independent → restart-deterministic), then write
/// the results back.
fn recompute_all(tx: &Connection) -> Result<(), StoreError> {
    let (nodes, admins) = load_graph(tx)?;
    let solved = solve_fixpoint(&nodes, &admins);

    let mut stmt =
        tx.prepare_cached("UPDATE events SET lamport = ?2, admin_seq = ?3 WHERE event_id = ?1")?;
    for (id, (lamport, admin_seq)) in &solved {
        stmt.execute(params![&id[..], lamport, admin_seq])?;
    }
    Ok(())
}

/// Load the full event graph and the per-room admin identity into memory.
#[allow(clippy::type_complexity)]
fn load_graph(
    tx: &Connection,
) -> Result<(BTreeMap<RawId, NodeStatic>, BTreeMap<RawId, RawId>), StoreError> {
    let mut nodes: BTreeMap<RawId, NodeStatic> = BTreeMap::new();
    {
        let mut stmt = tx.prepare("SELECT event_id, room_id, sender_id, event_type FROM events")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for r in rows {
            let (id, room, sender, event_type) = r?;
            nodes.insert(
                to_raw_id(&id)?,
                NodeStatic {
                    room: to_raw_id(&room)?,
                    sender: to_raw_id(&sender)?,
                    is_genesis: event_type == EventType::RoomCreated.as_str(),
                    parents: Vec::new(),
                },
            );
        }
    }

    {
        let mut stmt =
            tx.prepare("SELECT child_id, parent_id FROM event_parents ORDER BY child_id, ordinal")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for r in rows {
            let (child, parent) = r?;
            if let Some(node) = nodes.get_mut(&to_raw_id(&child)?) {
                node.parents.push(to_raw_id(&parent)?);
            }
        }
    }

    // Admin per room = the genesis sender; lowest event_id wins ties (matches
    // `admin_of_room`'s `ORDER BY event_id LIMIT 1`). BTreeMap iterates ascending.
    let mut admins: BTreeMap<RawId, RawId> = BTreeMap::new();
    for node in nodes.values() {
        if node.is_genesis {
            admins.entry(node.room).or_insert(node.sender);
        }
    }
    Ok((nodes, admins))
}

/// Iterate the derivation equations to their least fixpoint. Values only ever
/// rise from `None` toward a bounded final value, so this converges; the fixpoint
/// is unique and independent of iteration order.
fn solve_fixpoint(
    nodes: &BTreeMap<RawId, NodeStatic>,
    admins: &BTreeMap<RawId, RawId>,
) -> BTreeMap<RawId, (Option<i64>, Option<i64>)> {
    let mut vals: BTreeMap<RawId, (Option<i64>, Option<i64>)> =
        nodes.keys().map(|k| (*k, (None, None))).collect();
    loop {
        let mut changed = false;
        for (id, node) in nodes {
            let lamport = compute_lamport(node, &vals);
            let admin_seq = compute_admin(node, admins, &vals);
            if let Some(entry) = vals.get_mut(id) {
                if *entry != (lamport, admin_seq) {
                    *entry = (lamport, admin_seq);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    vals
}

/// `lamport` equation: genesis ⇒ 0; else `1 + max(parent lamports)` iff every
/// parent is present with a known lamport, else `None`.
fn compute_lamport(
    node: &NodeStatic,
    vals: &BTreeMap<RawId, (Option<i64>, Option<i64>)>,
) -> Option<i64> {
    if node.is_genesis && node.parents.is_empty() {
        return Some(0);
    }
    if node.parents.is_empty() {
        return None;
    }
    let mut max = -1_i64;
    for p in &node.parents {
        match vals.get(p) {
            Some((Some(l), _)) => max = max.max(*l),
            _ => return None, // parent dangling or lamport unknown
        }
    }
    Some(max + 1)
}

/// `admin_seq` equation: defined only for admin-authored events once the admin
/// (genesis) identity is known; genesis ⇒ 0; else `1 + max(defined parent
/// admin_seqs)`.
fn compute_admin(
    node: &NodeStatic,
    admins: &BTreeMap<RawId, RawId>,
    vals: &BTreeMap<RawId, (Option<i64>, Option<i64>)>,
) -> Option<i64> {
    let admin = admins.get(&node.room)?;
    if *admin != node.sender {
        return None;
    }
    if node.is_genesis {
        return Some(0);
    }
    let mut max: Option<i64> = None;
    for p in &node.parents {
        if let Some((_, Some(a))) = vals.get(p) {
            max = Some(max.map_or(*a, |m| m.max(*a)));
        }
    }
    max.map(|m| m + 1)
}

// ---------------------------------------------------------------------------
// Row mapping & small conversions
// ---------------------------------------------------------------------------

/// The raw `STORED_COLS` row as `rusqlite` yields it, before fallible conversion.
type RawEventRow = (Vec<u8>, Vec<u8>, Vec<u8>, String, Option<i64>, Option<i64>);

/// A raw `sync_parked` row `(event_id, wire, author_id, park_seq, depth)` as
/// `rusqlite` yields it, before fallible id/counter conversion.
type RawParkedRow = (Vec<u8>, Vec<u8>, Vec<u8>, i64, i64);

fn raw_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEventRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

/// Convert a raw `STORED_COLS` row into a [`StoredEvent`], surfacing corruption
/// as a typed error rather than a panic.
fn raw_to_stored(raw: RawEventRow) -> Result<StoredEvent, StoreError> {
    let (event_id, wire, room_id, event_type, lamport, admin_seq) = raw;
    let event_id = EventId::from_bytes(to_raw_id(&event_id)?);
    let wire = WireEvent::decode(&wire).map_err(StoreError::Decode)?;
    let room_id = RoomId::from_bytes(to_raw_id(&room_id)?);
    let event_type = EventType::from_registry(&event_type).ok_or_else(|| {
        StoreError::integrity(format!("unknown stored event_type {event_type:?}"))
    })?;
    Ok(StoredEvent {
        event_id,
        wire,
        room_id,
        event_type,
        lamport: lamport.map(sql_to_u64).transpose()?,
        admin_seq: admin_seq.map(sql_to_u64).transpose()?,
    })
}

/// Run a `STORED_COLS` query and collect the results.
fn stored_query<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<StoredEvent>, StoreError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params, raw_event_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(raw_to_stored(r?)?);
    }
    Ok(out)
}

/// Run a single-column id query and collect the ids.
fn id_query<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<EventId>, StoreError> {
    Ok(raw_id_query(conn, sql, params)?
        .into_iter()
        .map(EventId::from_bytes)
        .collect())
}

/// Run a single-column id query and collect raw ids.
fn raw_id_query<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<RawId>, StoreError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params, |row| row.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(to_raw_id(&r?)?);
    }
    Ok(out)
}

/// Narrow a stored BLOB to a 32-byte id, surfacing a wrong length as integrity
/// corruption (never a slice panic).
fn to_raw_id(bytes: &[u8]) -> Result<RawId, StoreError> {
    <RawId>::try_from(bytes).map_err(|_| {
        StoreError::integrity(format!(
            "expected {DIGEST_LEN}-byte id, got {}",
            bytes.len()
        ))
    })
}

/// A non-negative stored derived value as a `u64`.
fn sql_to_u64(v: i64) -> Result<u64, StoreError> {
    u64::try_from(v).map_err(|_| StoreError::integrity("derived column is negative"))
}

/// A `u64` counter as a `SQLite` INTEGER, surfacing an out-of-range value as a
/// typed integrity error rather than a silent wrap. Used for the v2 sync-cache
/// counters (`park_seq`, `admin_seq`, `suspect_tip_seq`) that *are* meaningful,
/// unlike the advisory `created_at` (which reinterprets, [`created_at_to_sql`]).
fn u64_to_sql(v: u64) -> Result<i64, StoreError> {
    i64::try_from(v).map_err(|_| StoreError::integrity("counter exceeds i64 range"))
}

/// Encode a list of ids as a canonical-CBOR byte array (the `trust_decisions`
/// `event_ids` column), reusing the event core's deterministic codec.
fn encode_id_array(ids: &[EventId]) -> Vec<u8> {
    cbor::encode(&CborValue::Array(
        ids.iter()
            .map(|id| CborValue::Bytes(id.as_bytes().to_vec()))
            .collect(),
    ))
}

/// Decode a canonical-CBOR id array, surfacing corruption as a typed integrity
/// error (never a panic on stored bytes, spec §9).
fn decode_id_array(bytes: &[u8]) -> Result<Vec<EventId>, StoreError> {
    let value = cbor::decode_canonical(bytes)
        .map_err(|_| StoreError::integrity("trust event_ids not canonical CBOR"))?;
    let items = value
        .as_array()
        .ok_or_else(|| StoreError::integrity("trust event_ids not a CBOR array"))?;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let b = it
            .as_bytes()
            .ok_or_else(|| StoreError::integrity("trust event_id not CBOR bytes"))?;
        out.push(EventId::from_bytes(to_raw_id(b)?));
    }
    Ok(out)
}

/// Map an advisory `created_at` (`u64` ms epoch) to the `SQLite` INTEGER domain.
/// `created_at` is display-only and never used for ordering or authorization
/// (Membership §2.3), so the reinterpretation of absurd (`> i64::MAX`) values is
/// immaterial and, critically, identical on insert and rebuild.
#[allow(clippy::cast_possible_wrap)]
fn created_at_to_sql(created_at: u64) -> i64 {
    created_at as i64
}

#[cfg(test)]
mod tests;
