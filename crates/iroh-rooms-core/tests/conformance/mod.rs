//! Protocol-conformance module tree and the authoritative traceability table.
//!
//! This binary is the **authoritative** §-indexed conformance suite for the
//! spike Protocol Test Vectors. `tests/golden_vectors.rs` (the IR-0002 landing)
//! stays in place as a flat, still-green regression grab-bag; the minor overlap
//! with the vectors ported here is accepted as cheap insurance (spec §8 / Open Q1
//! — default: keep both).
//!
//! ## Golden-value tiers (spec §4.3)
//!
//! * **Tier 1 — independently reproduced (authoritative).** The cast public keys,
//!   the 242-byte golden CSB + its `event_id` (`c389e2…85a1`), signature,
//!   `room_id_A` (`43c19f2e…16a3`), `room_id_B` (`cad9174a…3494`), the tampered
//!   id (`6267b72c…c75c`), and the cross-room re-signed id (`81b6a82b…f057`) are
//!   asserted against the exact spike hex. A mismatch is a hard NO-GO.
//! * **Tier 2 — regenerated & pinned.** The multi-event fixture-log ids
//!   (`E_create … E_pipe`) were **not** independently reproduced by the spike
//!   (their content maps were never pinned), so [`fixtures`] regenerates them from
//!   the landed content schema and pins the produced values as regression
//!   tripwires. See `fixtures.rs` for the divergence note.
//!
//! ## Vector → test map (all 20)
//!
//! | Vector | Test fn | Module |
//! |---|---|---|
//! | §1  canonical determinism        | `vector_01_canonical_serialization_determinism`   | serialization |
//! | §2  non-canonical rejected       | `vector_02_non_canonical_encoding_rejected`       | serialization |
//! | §3  `event_id` recomputed        | `vector_03_event_id_is_recomputed`                | serialization |
//! | §4  `room_id` bound (genesis)    | `vector_04_room_id_derivation_bound`              | serialization |
//! | §5  signature under device key   | `vector_05_signature_under_device_key`            | serialization |
//! | §6  tamper ⇒ id+sig fail          | `vector_06_tampered_field_breaks_id_and_signature`| serialization |
//! | §7  cross-room replay             | `vector_07_cross_room_replay_rejected`            | serialization |
//! | §8  duplicate idempotency         | `vector_08_duplicate_ignored_idempotently`        | `idempotency_ordering` |
//! | §9  out-of-order buffering        | `vector_09_child_before_parent_buffered`          | `idempotency_ordering` |
//! | §10 total order                   | `vector_10_deterministic_total_order`             | `idempotency_ordering` |
//! | §11 concurrent join/kick          | `vector_11_concurrent_join_kick_removed`          | membership |
//! | §12 equivocation                  | `vector_12_admin_equivocation_flagged`            | advisory |
//! | §13 non-member rejected           | `vector_13_non_member_event_rejected`             | membership |
//! | §14 insufficient role             | `vector_14_insufficient_role_rejected`            | membership |
//! | §15 stale invite / bad cap        | `vector_15_bad_capability_and_expired_invite`     | membership |
//! | §16 blob serve gate               | `vector_16_blob_serve_gate`                       | membership |
//! | §17 pipe connect gate             | `vector_17_pipe_connect_gate`                     | membership |
//! | §18 concurrent attributes         | `vector_18_concurrent_attributes_least_privilege` | membership |
//! | §19 leave then rejoin             | `vector_19_leave_consumes_invite`                 | membership |
//! | §20 clock skew advisory           | `vector_20_clock_skew_advisory_only`              | advisory |
//!
//! ## §8 taxonomy coverage (every outcome mapped)
//!
//! Rejections (15): `unknown_schema_version`, `unknown_event_type` → serialization;
//! `non_canonical_encoding` → §2; `id_mismatch` → §3/§6; `bad_signature` → §5/§6;
//! `unbound_device` → membership (`unbound_device_is_rejected`); `not_a_member`
//! → §13; `insufficient_role` → §14; `room_id_mismatch` → §4/§7; `invalid_content`
//! → serialization (incl. the `file.shared` semantic-bounds vectors,
//! `invalid_content_file_shared_*` / `valid_file_shared_*` — IR-0203);
//! `expired_invite` → §15/§19; `bad_capability` → §15; `room_full` → membership;
//! `too_many_parents`, `not_genesis_descended` → serialization.
//! Ignored (1): `duplicate` → §8. Advisory flags (3): `clock_skew` → §20;
//! `equivocation` → §12 (admin-fork detection pointer: `sync_convergence.rs`);
//! `from_removed_member` → membership (`from_removed_member_flag_on_removed_author`).
//!
//! The [`taxonomy`] gate (`every_reason_and_flag_is_covered_or_deferred`) enforces
//! that this table stays complete (AC5); `DEFERRED` is empty.
//!
//! ## `docs/protocol.md` reconciliation (IR-0302 / #37)
//!
//! [`docs_reference`] is the drift gate for the implementer reference doc: it
//! embeds `docs/protocol.md` (`include_str!`) and asserts the doc's reason/flag
//! codes, `constants.rs` bounds + context strings, Tier-1 golden ids, and
//! vector→test map all still match this suite and the landed code — the "runnable
//! & reconciled" half of the doc's own acceptance criteria.

pub mod fixtures;

mod advisory;
mod docs_reference;
mod idempotency_ordering;
mod membership;
mod serialization;
mod taxonomy;
