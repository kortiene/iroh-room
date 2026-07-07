-- Iroh Rooms v1 SQLite compatibility fixture schema.
--
-- This is the pre-IR-0201 store shape: authoritative events plus parent edges,
-- no schema-v2 sync-cache tables. Tests seed rows from events.txt, stamp
-- PRAGMA user_version = 1, then require the current EventStore opener to migrate
-- in place while preserving every event wire byte.

CREATE TABLE events (
    event_id    BLOB    NOT NULL PRIMARY KEY,
    wire        BLOB    NOT NULL,
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

CREATE INDEX idx_events_room_order ON events(room_id, lamport, event_id);
CREATE INDEX idx_events_room_type ON events(room_id, event_type);
CREATE INDEX idx_events_room_sender ON events(room_id, sender_id);
CREATE INDEX idx_events_room_device ON events(room_id, device_id);
CREATE INDEX idx_parents_parent ON event_parents(parent_id);
CREATE INDEX idx_events_admin_seq ON events(room_id, admin_seq)
    WHERE admin_seq IS NOT NULL;
CREATE INDEX idx_events_room_created ON events(room_id, created_at);

PRAGMA user_version = 1;
