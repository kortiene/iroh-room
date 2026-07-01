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

use rusqlite::{params, Connection, OptionalExtension};

use crate::event::constants::DIGEST_LEN;
use crate::event::content::EventType;
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::IdentityKey;
use crate::event::signed::{self, SignedEvent};
use crate::event::validate::ValidatedEvent;
use crate::event::wire::WireEvent;

pub use error::StoreError;
pub use model::{InsertOutcome, InsertStats, StoredEvent};

/// A raw 32-byte id as stored in a BLOB column.
type RawId = [u8; DIGEST_LEN];

/// The `events` columns a [`StoredEvent`] is built from, in select order.
const STORED_COLS: &str = "event_id, wire, room_id, event_type, lamport, admin_seq";

/// A synchronous, `rusqlite`-backed local event store.
///
/// Wraps a single [`Connection`]. Writes go through a transaction; reads use the
/// connection's prepared-statement cache. Not `Sync`; share across threads behind
/// your own `Mutex` if needed (spec §10, multi-connection pooling is future work).
pub struct EventStore {
    conn: Connection,
}

impl EventStore {
    /// Open (creating if absent) a store at `path`, applying pragmas and the
    /// idempotent schema migration.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] if the file cannot be opened, or
    /// [`StoreError::Migration`] if it carries a newer unknown schema version.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Open a private in-memory store (tests / ephemeral derivations).
    ///
    /// # Errors
    /// As [`EventStore::open`].
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, StoreError> {
        schema::apply_pragmas(&conn)?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
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
        let tx = self.conn.transaction()?;
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
        let tx = self.conn.transaction()?;
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

    /// The DAG heads of a room: stored events that no stored event cites as a
    /// parent (Membership §3.4 causal heads), ordered by `event_id`.
    ///
    /// # Errors
    /// [`StoreError::Sqlite`] on a DB error.
    pub fn heads(&self, room: &RoomId) -> Result<Vec<EventId>, StoreError> {
        id_query(
            &self.conn,
            "SELECT e.event_id FROM events e \
             WHERE e.room_id = ?1 \
               AND NOT EXISTS (SELECT 1 FROM event_parents p WHERE p.parent_id = e.event_id) \
             ORDER BY e.event_id",
            params![&room.as_bytes()[..]],
        )
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
        let tx = self.conn.transaction()?;
        rebuild_in_tx(&tx)?;
        tx.commit()?;
        Ok(())
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
