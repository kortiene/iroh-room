# CBOR and frame-parser fuzzing

Recorded run for the v0.1 GA self-review (§4, threat-model T17). The harness
is `fuzz/` (its own Cargo workspace, excluded from the root workspace) and the
workflow is `.github/workflows/fuzz.yml`. That workflow is not a required check.

## What is fuzzed

| Target | Function |
| --- | --- |
| `wire_event_decode` | `WireEvent::decode` |
| `signed_event_decode` | `SignedEvent::decode` |
| `sync_message_decode` | `SyncMessage::decode` |
| `room_invite_ticket` | `RoomInviteTicket::from_str` on valid UTF-8 |
| `frame_length_prefix` | `decode_length_prefix` in `crates/iroh-rooms-net/src/frame.rs` |

`read_frame` calls `decode_length_prefix` on the 4-byte length prefix before it
allocates a body. The synchronous function is what the fuzzer drives. It does
not allocate a body for an oversized declared length. Existing loopback
`read_frame` / `write_frame` tests still pass; this split does not change the
frame encoding or the CBOR decoders.

## How long CI runs each target

`.github/workflows/fuzz.yml` is a matrix, one job per target, nightly only:

- **pull_request**, and only when the diff touches `fuzz/`, the frame codec,
  `ticket.rs`, `sync/message.rs`, or `crates/iroh-rooms-core/src/event/**`:
  **60 seconds** per target (`-max_total_time=60`). This is a smoke that the
  harness still builds and does not crash immediately. It is not on the
  required PR path. `verify` stays on Rust 1.100.0 and does not build `fuzz/`.
- **schedule** (Mondays 07:15 UTC) and **workflow_dispatch**: **300 seconds**
  per target (`-max_total_time=300`). A manual dispatch may only raise that
  floor. The workflow rejects a campaign budget below 300.

Each job copies `fuzz/seeds/<target>/` into the corpus directory, runs
`cargo fuzz run`, and uploads `fuzz/corpus/<target>/` plus
`fuzz/artifacts/<target>/` (crash inputs, when any).

## Recorded local run

2026-11-19. `cargo-fuzz` 0.13.2, `libfuzzer-sys` 0.4.13, rustc
`1.102.0-nightly (65ce8611f 2026-11-18)`, AddressSanitizer (cargo-fuzz's
default). Command, per target, from `fuzz/`:

```sh
cargo +nightly fuzz run <target> -- -max_total_time=300 -timeout=15 -print_final_stats=1
```

Time, executions, and corpus are libFuzzer's own `Done` / `corp:` lines.
Crashes are files under `fuzz/artifacts/<target>/`.

| Target | Time (s) | Execs | Exec/s | Corpus | Crashes |
| --- | ---: | ---: | ---: | --- | ---: |
| `wire_event_decode` | 301 | 24,269,424 | 80,629 | 200 files, 17 KiB | 0 |
| `signed_event_decode` | 301 | 22,410,492 | 74,453 | 235 files, 40 KiB | 0 |
| `sync_message_decode` | 301 | 20,801,633 | 69,108 | 238 files, 29 KiB | 0 |
| `room_invite_ticket` | 502 | 32,036,652 | 63,818 | 57 files, 15,116 B | 0 |
| `frame_length_prefix` | 301 | 140,793,734 | 467,753 | 5 files, 21 B | 0 |

`room_invite_ticket` was started with the same `-max_total_time=300`. libFuzzer
reported 502 seconds (505 seconds of wall clock) and then exited 0. The other
four stopped at 301 seconds. No target wrote a crash artifact, and no decoder
bug was fixed from this run.

`frame_length_prefix` reached coverage 29 and a 5-entry corpus within the
first moments and stayed there. That matches the function: a 4-byte length, a
cap check, and a slice. The run still executed the oversized, truncated, empty,
and in-range paths under AddressSanitizer.

Checked-in seeds live in `fuzz/seeds/`. The corpus grown by this run is not
committed (`fuzz/corpus/` is gitignored); CI uploads it as an artifact.
