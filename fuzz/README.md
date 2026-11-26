# cargo-fuzz targets

Nightly + libFuzzer harnesses for the untrusted-input decoders. This directory
is its own Cargo workspace (`[workspace] members = ["."]`), and the root
workspace `exclude`s it, so `cargo test --workspace` and stable clippy never
build it.

`fuzz/Cargo.lock` stays in sync with the root `Cargo.lock`: copy the root lock
here, then add only `libfuzzer-sys`, `arbitrary`, and `jobserver`.

Seeds under `fuzz/seeds/` were produced from `build_room_created`,
`SyncMessage::encode`, and `RoomInviteTicket`'s `Display`, using fixed test
keys, plus a few hand-written malformed prefixes.

Targets:

| Target | Entry point |
| --- | --- |
| `wire_event_decode` | `WireEvent::decode` |
| `signed_event_decode` | `SignedEvent::decode` |
| `sync_message_decode` | `SyncMessage::decode` |
| `room_invite_ticket` | `RoomInviteTicket::from_str` on the raw UTF-8 token, and again after the bytes are wrapped as a CBOR body with a valid 4-byte BLAKE3 checksum |
| `frame_length_prefix` | `decode_length_prefix` (`read_frame` calls this) |

## Run locally

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --version 0.13.2 --locked
cd fuzz
# Optional: copy the checked-in seeds into the gitignored corpus dir.
for t in wire_event_decode signed_event_decode sync_message_decode room_invite_ticket frame_length_prefix; do
  mkdir -p "corpus/$t"
  cp -a "seeds/$t/." "corpus/$t/"
done
cargo +nightly fuzz run wire_event_decode -- -max_total_time=300
```

Crashes land in `fuzz/artifacts/<target>/`. A crash is a bug in the decoder:
fix it with a normal `cargo test` regression under the crate, not only by
keeping the artifact.

If `c++` is Clang and it cannot see libstdc++ headers, point the build at g++
(`CC=gcc CXX=g++`). rustc's lld also needs the link-time `libstdc++.so`; on
Debian/Ubuntu that unversioned symlink lives under
`/usr/lib/gcc/x86_64-linux-gnu/<gcc-version>`, so set `LIBRARY_PATH` to that
directory when the link fails with `unable to find library -lstdc++`. GitHub's
`ubuntu-latest` image uses g++ as `c++` and does not need either variable.

CI (`.github/workflows/fuzz.yml`) is not a required check. Pull requests that
touch the parsers or this directory get a 60-second smoke per target. The
weekly schedule and `workflow_dispatch` run each target for 300 seconds.
Recorded run results: [`docs/security/fuzz-cbor-frame.md`](../docs/security/fuzz-cbor-frame.md).
