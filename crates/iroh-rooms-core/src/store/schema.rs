//! The `SQLite` schema (`user_version = 2`) and the idempotent migration.
//!
//! `events.event_id` + `events.wire` are **authoritative** (the append-only
//! source of truth, PRD §12 / ADR-2); every other column and the entire
//! `event_parents` table is a **derived cache** rebuildable from `wire`
//! (spec D4). All id columns are raw 32-byte BLOBs so `SQLite`'s `memcmp`
//! ordering *is* the protocol's bytewise `(lamport, event_id)` tie-break
//! (spec D3, Membership §2.1).
//!
//! **Schema v2 (IR-0201).** Adds five **derived-cache** tables realizing the two
//! PRD §12 sync tables (`sync_state`, `trust_decisions`) as physical rows:
//! `sync_state`, `sync_backfill_tokens`, `sync_parked`, `sync_parked_missing`,
//! `trust_decisions`. They persist the sync engine's genuinely non-rebuildable
//! transient state (the orphan park, the unconfirmed admin-tip suspicion, the
//! per-author backfill token buckets, and the equivocation audit trail) so a
//! **process restart** cannot lose in-flight buffering or silently clear a
//! fail-closed access gate (harden-recent-history-sync §1–§4). The migration is
//! **forward-only and additive**: `events` / `event_parents` are untouched, the
//! new tables are created empty, and the five tables remain droppable caches
//! re-derivable from `events` + reconnect (spec D1/D7).

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::error::StoreError;

/// The schema version this build creates and understands.
pub(crate) const USER_VERSION: i64 = 2;

/// Connection pragmas applied on every open (spec §5).
///
/// `WAL` gives multi-reader + single-writer; `foreign_keys = ON` enforces the
/// `event_parents.child_id` FK; `synchronous = NORMAL` is the WAL-recommended
/// durability/throughput balance.
///
/// `busy_timeout` is deliberately **not** in this batch: it is set separately via
/// `Connection::busy_timeout` (see [`super::StoreOptions`] / `EventStore::open_with`)
/// so it can be parameterized per-open and cleared for the fail-fast opt-out
/// (issue #85). Every `EventStore` write transaction also uses `BEGIN IMMEDIATE`
/// (`EventStore::begin_write`) rather than `BEGIN DEFERRED`, so the busy handler
/// covers the write-lock wait for read-then-write bodies too.
const PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA foreign_keys = ON;
    PRAGMA synchronous = NORMAL;
";

/// The `CREATE TABLE/INDEX IF NOT EXISTS` DDL (spec §5). Idempotent: re-running
/// it on an already-migrated database is a no-op.
const DDL: &str = "
CREATE TABLE IF NOT EXISTS events (
    event_id    BLOB    NOT NULL PRIMARY KEY,
    wire        BLOB    NOT NULL,
    -- ---- derived cache below this line ----
    room_id     BLOB    NOT NULL,
    sender_id   BLOB    NOT NULL,
    device_id   BLOB    NOT NULL,
    event_type  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    lamport     INTEGER,
    admin_seq   INTEGER
) STRICT;

CREATE TABLE IF NOT EXISTS event_parents (
    child_id    BLOB    NOT NULL,
    parent_id   BLOB    NOT NULL,
    ordinal     INTEGER NOT NULL,
    PRIMARY KEY (child_id, ordinal),
    FOREIGN KEY (child_id) REFERENCES events(event_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS idx_events_room_order   ON events(room_id, lamport, event_id);
CREATE INDEX IF NOT EXISTS idx_events_room_type    ON events(room_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_room_sender  ON events(room_id, sender_id);
CREATE INDEX IF NOT EXISTS idx_events_room_device  ON events(room_id, device_id);
CREATE INDEX IF NOT EXISTS idx_parents_parent      ON event_parents(parent_id);
CREATE INDEX IF NOT EXISTS idx_events_admin_seq    ON events(room_id, admin_seq)
    WHERE admin_seq IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_room_created  ON events(room_id, created_at);

-- ======================================================================
-- Schema v2 (IR-0201): sync durability derived-cache tables.
-- Every table below is a DERIVED CACHE: droppable and re-derivable from the
-- authoritative `events` table + reconnect (spec D1). `events`/`event_parents`
-- are unchanged; a v1 database upgrades in place with these created empty (D7).
-- ======================================================================

-- Per-room sync cursor + unconfirmed-admin-tip state (single row per room).
CREATE TABLE IF NOT EXISTS sync_state (
    room_id             BLOB    NOT NULL PRIMARY KEY,      -- 32 bytes
    -- recent-chat cursor: advisory optimization only; NULL = none yet (OQ-1).
    chat_cursor_lamport INTEGER,
    chat_cursor_event   BLOB,                              -- 32 bytes; tie-break with lamport
    -- unconfirmed higher admin tip advertised but not yet backfilled (spec D6).
    -- Its presence re-raises Completeness::AdminViewSuspect across a restart so a
    -- reboot cannot fail-open on a removal-sensitive gate (spec §1.1 / D3).
    suspect_tip_event    BLOB,                             -- 32 bytes; NULL = no suspicion
    suspect_tip_seq      INTEGER,                          -- admin_seq of the suspicion
    suspect_tip_attempts INTEGER NOT NULL DEFAULT 0,       -- remaining attempts (bounded by config)
    updated_at           INTEGER NOT NULL                  -- advisory/debug only
) STRICT;

-- Per-(room, author) backfill token bucket (anti-amplification; spec §4.4/§6.3).
-- Persisted so a crash-loop cannot reset the amplification budget (spec §1.3/R4).
CREATE TABLE IF NOT EXISTS sync_backfill_tokens (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    author_id   BLOB    NOT NULL,                          -- 32 bytes
    tokens      INTEGER NOT NULL,                          -- current bucket level
    PRIMARY KEY (room_id, author_id)
) STRICT;

-- The orphan park: causally-incomplete-but-plausible frames awaiting backfill.
-- `wire` is the only load-bearing column (re-validated on load, D5); the rest are
-- re-derivable but stored to avoid re-decoding on the eviction path.
CREATE TABLE IF NOT EXISTS sync_parked (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    event_id    BLOB    NOT NULL,                          -- 32 bytes (parked frame id)
    wire        BLOB    NOT NULL,                          -- verbatim WireEvent bytes
    author_id   BLOB    NOT NULL,                          -- 32 bytes (per-author cap key)
    park_seq    INTEGER NOT NULL,                          -- monotone arrival order (eviction key)
    depth       INTEGER NOT NULL DEFAULT 0,                -- backfill chain depth chased
    PRIMARY KEY (room_id, event_id)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_parked_room_seq    ON sync_parked(room_id, park_seq);
CREATE INDEX IF NOT EXISTS idx_parked_room_author ON sync_parked(room_id, author_id);

-- The missing parents each parked frame is waiting on (drives the WantEvents
-- retry on open, spec §6.3). Cascades when its parked frame is deleted.
CREATE TABLE IF NOT EXISTS sync_parked_missing (
    room_id     BLOB    NOT NULL,
    event_id    BLOB    NOT NULL,                          -- the parked child
    missing_id  BLOB    NOT NULL,                          -- a parent it is waiting for
    PRIMARY KEY (room_id, event_id, missing_id),
    FOREIGN KEY (room_id, event_id) REFERENCES sync_parked(room_id, event_id) ON DELETE CASCADE
) STRICT;

-- Append-only equivocation / incompleteness audit trail (PRD §13.2, §16.3).
-- Survives restart so a reboot cannot erase a CRITICAL admin-fork alert (D6).
CREATE TABLE IF NOT EXISTS trust_decisions (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    seq         INTEGER NOT NULL,                          -- per-room monotone insertion order
    code        TEXT    NOT NULL,                          -- 'equivocation' | 'admin_view_suspect'
    severity    TEXT    NOT NULL,                          -- 'critical' | 'warning'
    admin_seq   INTEGER,                                   -- the contested admin_seq (if any)
    event_ids   BLOB    NOT NULL,                          -- CBOR array of the implicated raw ids
    created_at  INTEGER NOT NULL,                          -- advisory/debug only
    PRIMARY KEY (room_id, seq)
) STRICT;

-- ======================================================================
-- Schema v2 extension (#191 step 6): persisted epoch key store.
-- This is a DERIVED CACHE: the keys can be re-obtained by unwrapping the
-- `member.key_distribution` / `member.removed.rotation` events stored in
-- `events`, so the table is droppable and re-derivable. It persists only
-- successfully adopted epoch keys (not conflict candidates), so a restart
-- does not re-poison an epoch that was already resolved.
-- ======================================================================

CREATE TABLE IF NOT EXISTS room_keys (
    room_id         BLOB    NOT NULL,                      -- 32 bytes
    epoch           INTEGER NOT NULL,                      -- key_epoch
    key             BLOB    NOT NULL,                      -- 32-byte room_key
    source_event_id BLOB    NOT NULL,                      -- 32-byte adopting event_id
    PRIMARY KEY (room_id, epoch)
) STRICT;
";

/// Apply connection pragmas. `journal_mode = WAL` returns a row, so it is run via
/// `pragma_update`/`query` rather than `execute_batch` to avoid the "Execute
/// returned results" error.
pub(crate) fn apply_pragmas(conn: &Connection) -> Result<(), StoreError> {
    // `execute_batch` tolerates the WAL row result for the whole batch.
    conn.execute_batch(PRAGMAS)?;
    Ok(())
}

/// Create the schema if absent and stamp `user_version`, idempotently.
///
/// # Errors
/// [`StoreError::Migration`] if an existing database carries a newer, unknown
/// `user_version`; [`StoreError::Sqlite`] on any DDL failure.
pub(crate) fn migrate(conn: &Connection) -> Result<(), StoreError> {
    refuse_newer_user_version(conn)?;
    conn.execute_batch(DDL)?;
    // `?` binding is not allowed in PRAGMA; USER_VERSION is a trusted constant.
    conn.pragma_update(None, "user_version", USER_VERSION)?;
    Ok(())
}

/// Refuse a store whose `user_version` is newer than [`USER_VERSION`].
///
/// The read is `PRAGMA user_version` only: it does not change `journal_mode`
/// and does not rewrite header bytes 18–19. Callers must run this before
/// [`apply_pragmas`].
///
/// # Errors
/// [`StoreError::Migration`] when `user_version` is newer than this binary.
/// [`StoreError::Sqlite`] when the pragma cannot be read.
pub(crate) fn refuse_newer_user_version(conn: &Connection) -> Result<(), StoreError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > USER_VERSION {
        return Err(StoreError::Migration(newer_user_version_message(current)));
    }
    Ok(())
}

/// Read `user_version` from `path` without altering the file, and refuse a
/// newer store. A missing or empty path is left for the caller to create.
///
/// The probe is a read-only `immutable=1` open, so it does not change
/// `journal_mode`, does not rewrite header bytes 18–19, and still works when
/// the data directory cannot be written. That keeps a read-only refusal on
/// the version error instead of a failed WAL pragma.
///
/// # Errors
/// [`StoreError::Migration`] when the on-disk `user_version` is newer than
/// this binary. [`StoreError::Sqlite`] when an existing database cannot be read.
pub(crate) fn refuse_newer_user_version_at(path: &Path) -> Result<(), StoreError> {
    if !path.is_file() {
        return Ok(());
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() == 0 => return Ok(()),
        Ok(_) => {}
        // Let the real open report a path that disappeared or cannot be stat'd.
        Err(_) => return Ok(()),
    }
    let current = read_user_version_without_writing(path)?;
    if current > USER_VERSION {
        return Err(StoreError::Migration(newer_user_version_message(current)));
    }
    Ok(())
}

/// Pinned prefix, then the recovery hint from `docs/compatibility.md`
/// (restore from backup, or retry with the previous binary).
fn newer_user_version_message(current: i64) -> String {
    format!(
        "database user_version {current} is newer than supported {USER_VERSION}; \
         restore from backup or retry with the previous binary"
    )
}

/// `PRAGMA user_version` via an open that cannot rewrite the database.
fn read_user_version_without_writing(path: &Path) -> Result<i64, StoreError> {
    // A plain read-only open still does not set `journal_mode`. Used when the
    // URI probe cannot parse the path.
    let conn = if let Ok(conn) = open_immutable_readonly(path) {
        conn
    } else {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        Connection::open_with_flags(path, flags)?
    };
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn open_immutable_readonly(path: &Path) -> Result<Connection, rusqlite::Error> {
    let uri = sqlite_immutable_uri(path);
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(uri, flags)
}

/// `file://` URI with `mode=ro&immutable=1`. SQLite then reads the header and
/// does not create a journal, a WAL, or a shared-memory file.
fn sqlite_immutable_uri(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    // `file://` + `/abs/path` is `file:///abs/path` (empty authority).
    let mut uri = String::from("file://");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        for &byte in absolute.as_os_str().as_bytes() {
            push_uri_byte(&mut uri, byte);
        }
    }
    #[cfg(not(unix))]
    {
        // `file:///C:/...` — one more slash so the drive letter is not a host.
        uri.push('/');
        for byte in absolute.to_string_lossy().bytes() {
            if byte == b'\\' {
                push_uri_byte(&mut uri, b'/');
            } else {
                push_uri_byte(&mut uri, byte);
            }
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    uri
}

fn push_uri_byte(uri: &mut String, byte: u8) {
    match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
            uri.push(char::from(byte));
        }
        _ => {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}
