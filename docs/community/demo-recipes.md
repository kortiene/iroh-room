# Community Demo Recipes

These recipes are for the first builder cohort. They intentionally use explicit
data directories so participants do not mix beta data with personal state.

Supported binary artifact for `v0.1.0-rc.3`: `x86_64-apple-darwin`. Other
builders can build from source.

Linux x86_64 builders should use the exact release tag rather than `main`. The
shipping dependency graph requires Rust 1.85 or newer in practice; RC1 was built
with Rust 1.96.0.

```bash
git clone --branch v0.1.0-rc.3 --depth 1 \
  https://github.com/kortiene/iroh-room.git
cd iroh-room
cargo build --locked --release -p iroh-rooms-cli
export PATH="$PWD/target/release:$PATH"
iroh-rooms --version
```

Expected: `iroh-rooms 0.1.0-rc.3`. Do not continue with a different version.

Before running any recipe:

```bash
iroh-rooms --version
```

Expected:

```text
iroh-rooms 0.1.0-rc.3
```

## Safety Rules

- Treat invite tickets as passwords.
- Do not paste full tickets into public issues.
- Do not attach `rooms.db`, blob files, `identity.secret`, full backups, or
  unredacted `audit.ndjson`.
- Use trusted local machines only. Local data is plaintext in this beta.
- Stop all long-running `iroh-rooms` processes before deleting data dirs.

## Recipe 1: Two Humans Join A Private Room

Goal: Alice creates a room, Bob joins it, and both can see membership.

### Alice Terminal

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/alice"
mkdir -p "$IROH_ROOMS_HOME"

iroh-rooms identity create --name "Alice"
iroh-rooms identity show
iroh-rooms room create "Cohort Room"
```

Save:

- Alice identity id as `<ALICE_ID>`.
- Room id as `<ROOM_ID>`.

### Bob Terminal

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/bob"
mkdir -p "$IROH_ROOMS_HOME"

iroh-rooms identity create --name "Bob"
iroh-rooms identity show
```

Send Bob's identity id to Alice over a private channel as `<BOB_ID>`.

### Alice Terminal

```bash
iroh-rooms room invite <ROOM_ID> --invitee <BOB_ID> --expires 24h
```

Send the `roomtkt1...` ticket to Bob over a private channel.

Start Alice's join host after the invite exists:

```bash
iroh-rooms room tail <ROOM_ID> --accept-joins -v
```

Keep this process running while Bob joins. If `-v` prints a `listening:`
address, Bob can pass it as `--peer <ALICE_ENDPOINT_OR_ADDR>`.

### Bob Terminal

```bash
iroh-rooms room join <BOB_TICKET>
```

If discovery fails, retry with Alice's printed peer address:

```bash
iroh-rooms room join <BOB_TICKET> --peer <ALICE_ENDPOINT_OR_ADDR>
```

### Verify

Alice:

```bash
iroh-rooms room members <ROOM_ID>
```

Bob:

```bash
iroh-rooms room members <ROOM_ID>
iroh-rooms room send <ROOM_ID> "hello from Bob"
```

Alice's running `room tail` should observe Bob's message.

Before continuing to Recipe 2 or Recipe 3, stop Alice's running `room tail`
with Ctrl-C. The beta CLI uses local store and blob-store locks; leaving a
long-running Alice process open can make the next Alice command fail with
`blob_store_locked`.

### Common Failures

- `no_admin_reachable`: Alice started `room tail --accept-joins` before creating
  the invite, Alice is not running `room tail <ROOM_ID> --accept-joins`, or Bob
  needs `--peer`.
- `ticket_*`: the ticket was truncated or copied incorrectly.
- `expired_invite`: ask Alice to mint a new invite.

## Recipe 2: Share And Fetch A Verified File

Goal: Alice shares a file reference. Bob fetches it from Alice and verifies the
content hash.

Prerequisite: complete Recipe 1.

### Alice Terminal

If Alice still has a `room tail` or `pipe expose` process running from a prior
recipe, stop it with Ctrl-C before sharing the file.

Create a small file:

```bash
printf 'hello from iroh rooms\n' > hello.txt
iroh-rooms file share <ROOM_ID> ./hello.txt
```

Save the printed `file_...` handle as `<FILE_ID>`.

Start serving the room and local blob store:

```bash
iroh-rooms room tail <ROOM_ID> -v
```

Keep this process running while Bob fetches.

### Bob Terminal

Fetch directly from Alice. `file fetch` connects to the peer and performs a
bounded wait for the `file.shared` event when Bob has not learned it yet. Do not
wait for the live `room tail` to display the share: RC1's live renderer shows
`message.text` only.

```bash
mkdir -p ./downloads
iroh-rooms file fetch <ROOM_ID> <FILE_ID> --out ./downloads --peer <ALICE_ENDPOINT_OR_ADDR>
```

### Verify

```bash
cat ./downloads/hello.txt
```

Expected:

```text
hello from iroh rooms
```

### Common Failures

- `blob_unavailable`: Alice is not running `room tail`, or the wrong peer is
  serving.
- `blob_store_locked`: another Alice process is still using the blob store.
  Stop Alice's prior `room tail` or `pipe expose` process, then retry.
- `no_such_file`: Bob could not learn the `file.shared` event within the bounded
  wait. Confirm Alice is serving the same room and retry with the exact
  `listening:` address passed through `--peer`.
- `hash_mismatch`: do not trust the file. File a redacted issue.

## Recipe 3: Share A Localhost Preview With Live Pipe

Goal: Alice exposes a local web app to Bob without publishing a public URL.

Prerequisite: complete Recipe 1.

### Alice Terminal

If Alice still has a `room tail` process running from a prior recipe, stop it
with Ctrl-C before exposing the pipe.

Start a local test server:

```bash
mkdir -p .cohort/site
printf '<h1>Hello from private Live Pipe</h1>\n' > .cohort/site/index.html
python3 -m http.server 3000 --bind 127.0.0.1 --directory .cohort/site
```

Leave it running.

### Alice Second Terminal

Use Alice's data dir:

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/alice"

iroh-rooms pipe expose <ROOM_ID> \
  --tcp 127.0.0.1:3000 \
  --allow <BOB_ID> \
  --label cohort-preview \
  -v
```

Save:

- printed pipe id as `<PIPE_ID>`;
- printed listening peer address as `<ALICE_ENDPOINT_OR_ADDR>` if available.

Keep this process running.

### Bob Terminal

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/bob"

iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local 3001
```

If discovery fails:

```bash
iroh-rooms pipe connect <ROOM_ID> <PIPE_ID> --local 3001 --peer <ALICE_ENDPOINT_OR_ADDR>
```

Then open:

```text
http://127.0.0.1:3001
```

### Verify

Bob should see:

```text
Hello from private Live Pipe
```

### Common Failures

- `peer_offline`: Alice's `pipe expose` process is not running.
- `peer_unauthorized`: Bob's identity id is not in Alice's `--allow` list.
- `blob_store_locked`: another Alice process is still using the blob store.
  Stop Alice's prior `room tail` process, then retry `pipe expose`.
- local port already used: choose another `--local` port, such as `3002`.

## Optional Recipe 4: Agent Status

Goal: an explicitly invited agent posts signed status to the room.

Create a third local identity:

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/agent"
mkdir -p "$IROH_ROOMS_HOME"

iroh-rooms identity create --name "build-agent"
iroh-rooms identity show
```

Send the agent identity id to Alice as `<AGENT_ID>`.

Alice invites the agent:

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/alice"
iroh-rooms agent invite <ROOM_ID> <AGENT_ID>
```

Alice must host joins while the agent redeems the ticket:

```bash
iroh-rooms room tail <ROOM_ID> --accept-joins -v
```

Agent joins:

```bash
export IROH_ROOMS_HOME="$PWD/.cohort/agent"
iroh-rooms room join <AGENT_TICKET> --peer <ALICE_ENDPOINT_OR_ADDR>
```

Agent posts status:

```bash
iroh-rooms agent status <ROOM_ID> running_tests \
  --message "Running the cohort recipe" \
  --progress 40
```

Alice or Bob can read the local log:

```bash
iroh-rooms room tail <ROOM_ID> --offline
```

## Cleanup

Stop all running `iroh-rooms` and `python3 -m http.server` processes first.

Then remove demo data:

```bash
rm -rf .cohort hello.txt downloads
```

This deletes local identity secrets and room state for the demo.
