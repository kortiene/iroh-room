-- Iroh Rooms v0.1.0-rc.3 SQLite compatibility fixture schema (P0.7).
--
-- This is the on-disk store shape written by the PUBLISHED v0.1.0-rc.3 binary
-- (iroh-rooms-0.1.0-rc.3-aarch64-unknown-linux-gnu-71fbb5007bef), captured
-- verbatim from `sqlite_master` of an rc.3-created rooms.db. rc.3 already
-- stamps schema v2, so unlike fixtures/v1 this fixture must survive the current
-- opener as a NO-OP migration: user_version stays 2, no table is recreated, and
-- every event wire byte is preserved. Tests seed rows from rc3/events.txt.

CREATE TABLE events (
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
CREATE TABLE event_parents (
    child_id    BLOB    NOT NULL,
    parent_id   BLOB    NOT NULL,
    ordinal     INTEGER NOT NULL,
    PRIMARY KEY (child_id, ordinal),
    FOREIGN KEY (child_id) REFERENCES events(event_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX idx_events_room_order   ON events(room_id, lamport, event_id);
CREATE INDEX idx_events_room_type    ON events(room_id, event_type);
CREATE INDEX idx_events_room_sender  ON events(room_id, sender_id);
CREATE INDEX idx_events_room_device  ON events(room_id, device_id);
CREATE INDEX idx_parents_parent      ON event_parents(parent_id);
CREATE INDEX idx_events_admin_seq    ON events(room_id, admin_seq)
    WHERE admin_seq IS NOT NULL;
CREATE INDEX idx_events_room_created  ON events(room_id, created_at);
CREATE TABLE sync_state (
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
CREATE TABLE sync_backfill_tokens (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    author_id   BLOB    NOT NULL,                          -- 32 bytes
    tokens      INTEGER NOT NULL,                          -- current bucket level
    PRIMARY KEY (room_id, author_id)
) STRICT;
CREATE TABLE sync_parked (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    event_id    BLOB    NOT NULL,                          -- 32 bytes (parked frame id)
    wire        BLOB    NOT NULL,                          -- verbatim WireEvent bytes
    author_id   BLOB    NOT NULL,                          -- 32 bytes (per-author cap key)
    park_seq    INTEGER NOT NULL,                          -- monotone arrival order (eviction key)
    depth       INTEGER NOT NULL DEFAULT 0,                -- backfill chain depth chased
    PRIMARY KEY (room_id, event_id)
) STRICT;
CREATE INDEX idx_parked_room_seq    ON sync_parked(room_id, park_seq);
CREATE INDEX idx_parked_room_author ON sync_parked(room_id, author_id);
CREATE TABLE sync_parked_missing (
    room_id     BLOB    NOT NULL,
    event_id    BLOB    NOT NULL,                          -- the parked child
    missing_id  BLOB    NOT NULL,                          -- a parent it is waiting for
    PRIMARY KEY (room_id, event_id, missing_id),
    FOREIGN KEY (room_id, event_id) REFERENCES sync_parked(room_id, event_id) ON DELETE CASCADE
) STRICT;
CREATE TABLE trust_decisions (
    room_id     BLOB    NOT NULL,                          -- 32 bytes
    seq         INTEGER NOT NULL,                          -- per-room monotone insertion order
    code        TEXT    NOT NULL,                          -- 'equivocation' | 'admin_view_suspect'
    severity    TEXT    NOT NULL,                          -- 'critical' | 'warning'
    admin_seq   INTEGER,                                   -- the contested admin_seq (if any)
    event_ids   BLOB    NOT NULL,                          -- CBOR array of the implicated raw ids
    created_at  INTEGER NOT NULL,                          -- advisory/debug only
    PRIMARY KEY (room_id, seq)
) STRICT;

PRAGMA user_version = 2;
