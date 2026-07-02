# IR-0006 — rolled-up comparison results

Rendered from `transport-probe compare|late-join|admission|admin-tip` runs on
loopback (`RelayMode::Disabled`), `iroh = 1.0.1`, `iroh-gossip = 0.101.0`, one
signing identity, workload = 1 `room.created` genesis + 10 `message.text`
events (11 events total). Regenerate with:

```
cargo run -p spike-transport --bin transport-probe -- compare --n <N> --events 10 --json
cargo run -p spike-transport --bin transport-probe -- late-join --backend <mesh|gossip> --n 3 --events 10
cargo run -p spike-transport --bin transport-probe -- admission --backend <mesh|gossip>
cargo run -p spike-transport --bin transport-probe -- admin-tip
```

## Steady-state fan-out (AC1 — N=2..5 propagation)

| backend | n | events | converged | prop min (ms) | prop median (ms) | prop max (ms) | fanout completion (ms) | late-join gap | admission enforced | interloper received | lagged events |
|---------|---|--------|-----------|---------------|-------------------|---------------|------------------------|---------------|---------------------|----------------------|---------------|
| mesh    | 2 | 11 | yes | 15 | 16 | 18 | 186 | — | — | — | 0 |
| gossip  | 2 | 11 | yes | 15 | 17 | 17 | 187 | — | — | — | 0 |
| mesh    | 3 | 11 | yes | 16 | 16 | 18 | 185 | — | — | — | 0 |
| gossip  | 3 | 11 | yes | 15 | 16 | 17 | 183 | — | — | — | 0 |
| mesh    | 4 | 11 | yes | 15 | 16 | 16 | 180 | — | — | — | 0 |
| gossip  | 4 | 11 | yes | 15 | 16 | 17 | 181 | — | — | — | 0 |
| mesh    | 5 | 11 | yes | 15 | 16 | 16 | 179 | — | — | — | 0 |
| gossip  | 5 | 11 | yes | 15 | 16 | 17 | 182 | — | — | — | 0 |

Both backends converge to full set equality at every N, on loopback, with
propagation latency indistinguishable within run-to-run noise (all single-digit
ms differences). This is the expected result at N≤5 loopback (spec §12 risk:
"a small/no latency gap is a valid confirming result, not a measurement
flaw") — mesh's 1-hop direct link and gossip's PlumTree are both effectively
1-hop at this size. `fanout_completion_ms` is dominated by the fixed
per-publish convergence poll interval (15ms) × 11 published events, not by
backend transport cost — see `NOTES.md` §2 for the reading.

## Late-join (AC2 — history gap)

| backend | pre-join n | events published | received by newcomer | gap |
|---------|------------|-------------------|------------------------|-----|
| mesh    | 3          | 11                 | 0                       | 11  |
| gossip  | 3          | 11                 | 0                       | 11  |

Both transports give the late-joiner **zero** raw history — confirms the
transport-agnostic half of the claim (§4: "neither raw transport provides
room-wide history"). The asymmetry is what each transport lets the *app* do
about it next: mesh's newcomer already holds an authenticated bidi link to
every existing member, so a backfill pull is one more frame; gossip's
newcomer has no per-peer connection to attach a pull to at all.

## Admission (AC3 — auth model)

| backend | scenario | result |
|---------|----------|--------|
| mesh    | interloper connects with an `EndpointId` outside the allowlist | refused **before** `accept_bi()` (admission enforced = yes) |
| gossip  | interloper subscribes knowing only the 32-byte `TopicId` | admitted with no auth check; published an event a room member received (interloper received = yes) |

## Admin-tip carrier freshness (Residual Open Decision 13)

| carrier | mechanism | freshness (send/broadcast → peer observes) |
|---------|-----------|---------------------------------------------|
| mesh    | `SyncMessage::AdminTip` as a `TAG_CONTROL` frame on the existing authenticated bidi link | 18–21 ms |
| gossip  | `AdminTip` broadcast on a dedicated liveness `TopicId`, off the event critical path | 3–6 ms |

Gossip's broadcast is faster in this 2-node loopback probe (no per-peer
polling loop, direct neighbor push), but both are within noise of each other
at this N and both are far under any liveness-detection threshold that would
matter (seconds, not tens of ms). See `NOTES.md` §5 for the decision.

## Implementation complexity

| backend | backend LOC (`mesh.rs` / `gossip.rs`, excl. `#[cfg(test)]`) | 0.x crates added |
|---------|---------------------------------------------------------------|---------------------|
| mesh    | 405 | 0 |
| gossip  | 299 | 1 direct (`iroh-gossip`) + `futures-concurrency` transitively (the only wholly new crate to the workspace graph — every other `iroh-gossip` transitive dependency, incl. `irpc`, `n0-future`, `tokio-util`, `rand`, `hex`, `indexmap`, `derive_more`, is already pulled in by `iroh`/`iroh-blobs`) |

Gossip's *own* backend is ~100 fewer lines than mesh's (no dial-fan-out, no
per-peer connection bookkeeping), but that is not a fair complexity picture —
see `NOTES.md` §4 for what each backend does *not* implement.
