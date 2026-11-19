# v0.1.0-rc.6 compatibility fixture

Harvested on 2026-11-17 (2026-11-17T18:13:23Z) from a `rooms.db` written by the
published `v0.1.0-rc.6` `x86_64-unknown-linux-gnu` binary through its CLI.
These bytes are the oracle. They were not regenerated from source.

A re-harvest does **not** reproduce these bytes. The published binary creates
random identity keys, so event ids, wire bytes, and the room id differ every
run. Only the procedure below is reproducible.

No secrets are in this directory. `identity.secret`, device seeds, and the
invite ticket were left in the temp homes and were not copied. `member.invited`
carries only a capability hash. The harvested `room_keys` table was empty.

## Binary

- Asset: `iroh-rooms-0.1.0-rc.6-x86_64-unknown-linux-gnu-bf353b78f63b.tar.gz`
- Archive SHA-256: `30cc5af50a788df8aeea8526abccb7b3f533dc8357752754ed261fe4d254a67f`
- Extracted `iroh-rooms` binary SHA-256: `78dfad408cfcf03569c5d023b36bd144dee4de1196e7a9e3e34c8637d2c7e80c`
- Source commit: `bf353b78f63b08ff5e3f425273aa3e0983167679`
- `iroh-rooms --version` printed `iroh-rooms 0.1.0-rc.6`

## Harvest commands

`$REPO` is a checkout that contains `scripts/verify-release-artifacts.sh`.
`$ROOT` is the temp harvest tree. `$BIN` is the extracted published binary.
`cosign` v3 must be on `PATH` (v3.1.4 was used). The verify command exited 0:
the per-archive `.sha256`, `SHA256SUMS`, `gh attestation verify` (archive
bundle and `SHA256SUMS`), and `cosign verify-blob` (`Verified OK`).

```bash
REPO=/path/to/iroh-room
ROOT=$(mktemp -d)
mkdir -p "$ROOT/assets" "$ROOT/unix-home" "$ROOT/data" "$ROOT/bob"

gh release download v0.1.0-rc.6 --repo kortiene/iroh-room --dir "$ROOT/assets" \
  --pattern 'iroh-rooms-0.1.0-rc.6-x86_64-unknown-linux-gnu-*' \
  --pattern 'SHA256SUMS' \
  --pattern 'SHA256SUMS.sigstore.json'

"$REPO/scripts/verify-release-artifacts.sh" \
  --dir "$ROOT/assets" \
  --commit bf353b78f63b08ff5e3f425273aa3e0983167679 \
  v0.1.0-rc.6 x86_64-unknown-linux-gnu

tar -xzf "$ROOT/assets/iroh-rooms-0.1.0-rc.6-x86_64-unknown-linux-gnu-bf353b78f63b.tar.gz" \
  -C "$ROOT/assets"
BIN="$ROOT/assets/iroh-rooms-0.1.0-rc.6-x86_64-unknown-linux-gnu-bf353b78f63b/iroh-rooms"

export HOME="$ROOT/unix-home"
export IROH_ROOMS_HOME="$ROOT/data"

"$BIN" --data-dir "$IROH_ROOMS_HOME" identity create --name "RC6 Continuity"
"$BIN" --data-dir "$ROOT/bob" identity create --name "Bob"
BOB_ID=$("$BIN" --data-dir "$ROOT/bob" identity show | awk -F': ' '/^identity_id:/ {print $2}')
ROOM=$("$BIN" --data-dir "$IROH_ROOMS_HOME" room create "RC6 Continuity Room" | awk -F': ' '/^room_id:/ {print $2}')
"$BIN" --data-dir "$IROH_ROOMS_HOME" room send "$ROOM" "rc.6 continuity fixture: first message"
"$BIN" --data-dir "$IROH_ROOMS_HOME" room send "$ROOM" "rc.6 continuity fixture: second message"
"$BIN" --data-dir "$IROH_ROOMS_HOME" room send "$ROOM" "rc.6 continuity fixture: third message"
"$BIN" --data-dir "$IROH_ROOMS_HOME" room invite "$ROOM" --invitee "$BOB_ID" --role member
```

The invite command prints a ticket. That stdout was discarded and is not in
`events.txt`. `--data-dir` pointed at the same directory as `IROH_ROOMS_HOME`.
Bob's home exists only to supply a public `identity_id`.

## Export queries

Schema (`store_rc6_schema.sql`): the `sqlite_master` SQL, then
`PRAGMA user_version = 2` (`PRAGMA user_version` on the harvested file was 2).

```bash
sqlite3 "$IROH_ROOMS_HOME/rooms.db" \
  "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid;"
```

Events (`events.txt`): one line per row, `label|event_type|blake3:<event_id hex>|wire hex`,
ordered by `(lamport, event_id)`. Labels are assigned in that order
(`E_CREATE`, then `E_MESSAGE_1` .. `E_MESSAGE_3`, then `E_INVITE_BOB`).
The derived-cache pairs recorded from those rows are `(E_CREATE, 0, 0)`,
`(E_MESSAGE_1, 1, 1)`, `(E_MESSAGE_2, 2, 2)`, `(E_MESSAGE_3, 3, 3)`,
`(E_INVITE_BOB, 4, 4)`.

```bash
sqlite3 "$IROH_ROOMS_HOME/rooms.db" <<'SQL'
SELECT
  CASE
    WHEN event_type = 'room.created' THEN 'E_CREATE'
    WHEN event_type = 'member.invited' THEN 'E_INVITE_BOB'
    WHEN event_type = 'message.text' THEN
      'E_MESSAGE_' || ROW_NUMBER() OVER (
        PARTITION BY event_type ORDER BY lamport, event_id
      )
  END
  || '|' || event_type
  || '|blake3:' || lower(hex(event_id))
  || '|' || lower(hex(wire))
FROM events
ORDER BY lamport, event_id;
SQL
```
