//! The `SQLite` schema (`user_version = 1`) and the idempotent migration.
//!
//! `events.event_id` + `events.wire` are **authoritative** (the append-only
//! source of truth, PRD §12 / ADR-2); every other column and the entire
//! `event_parents` table is a **derived cache** rebuildable from `wire`
//! (spec D4). All id columns are raw 32-byte BLOBs so `SQLite`'s `memcmp`
//! ordering *is* the protocol's bytewise `(lamport, event_id)` tie-break
//! (spec D3, Membership §2.1).

use rusqlite::Connection;

use super::error::StoreError;

/// The schema version this build creates and understands.
pub(crate) const USER_VERSION: i64 = 1;

/// Connection pragmas applied on every open (spec §5).
///
/// `WAL` gives multi-reader + single-writer; `foreign_keys = ON` enforces the
/// `event_parents.child_id` FK; `synchronous = NORMAL` is the WAL-recommended
/// durability/throughput balance.
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
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > USER_VERSION {
        return Err(StoreError::Migration(format!(
            "database user_version {current} is newer than supported {USER_VERSION}"
        )));
    }
    conn.execute_batch(DDL)?;
    // `?` binding is not allowed in PRAGMA; USER_VERSION is a trusted constant.
    conn.pragma_update(None, "user_version", USER_VERSION)?;
    Ok(())
}
