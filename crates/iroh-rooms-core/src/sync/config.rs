//! [`SyncConfig`] — the anti-amplification bounds and window defaults
//! (spec `bounded-recent-sync-prototype.md` §4.4).
//!
//! Every bound has a safe default tuned for the MVP target (≤5 peers, full mesh).
//! A Gate-D **NO-GO** condition is an *unbounded* orphan park or backfill, so the
//! engine enforces all of these and logs whenever one drops, evicts, or caps
//! something (spec §4.4 final paragraph / §9).
//!
//! These are anti-amplification bounds, **not** a room-size limit: nothing here
//! (or anywhere else in code) enforces or warns on the declared ≤5-peer ceiling
//! (ADR-1, `PHASE-0-SPIKE.md`). Above it these defaults do not degrade
//! gracefully — at N=25 delivery was measured to stop entirely (idle
//! `frames_sent=0`, `accepted=0`, 661 MB inbound backlog) while every
//! connectivity signal read healthy. Staying under the ceiling is the
//! operator's responsibility, documented in `README.md` and the PRD success
//! metrics.

/// Anti-amplification configuration for the sync engine (spec §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncConfig {
    /// Cap on parked (causally-incomplete) frames **per author key**; oldest is
    /// evicted on overflow (`MAX_PARKED_PER_AUTHOR`).
    pub max_parked_per_author: usize,
    /// Global cap on the orphan park across all authors (`MAX_PARKED_TOTAL`);
    /// oldest-first eviction on overflow. **Not an ordinary tuning knob:** a
    /// frame evicted here and never re-served is *permanently lost* — raising the
    /// bound moves that cliff, it does not remove it. Overflow is surfaced (never
    /// a silent drop): the first eviction each session records a CRITICAL
    /// `park_overflow` [`TrustDecision`](super::TrustDecision) naming the lost
    /// event, and every eviction bumps
    /// [`park_evicted`](super::SyncCounters::park_evicted).
    pub max_parked_total: usize,
    /// Max ids in a single `WantEvents`; larger needs are chunked
    /// (`MAX_BACKFILL_FANOUT_IDS`).
    pub max_backfill_fanout_ids: usize,
    /// Token-bucket capacity for backfill requests, keyed by the requesting
    /// (parked-frame) author (`BACKFILL_TOKENS_PER_AUTHOR`).
    pub backfill_tokens_per_author: u32,
    /// Tokens refilled into each author bucket per [`on_tick`](super::SyncEngine::on_tick).
    pub backfill_refill_per_tick: u32,
    /// Max consecutive missing-parent levels chased before a chain is treated as
    /// a phantom and dropped (`MAX_BACKFILL_DEPTH`). **Not an ordinary tuning
    /// knob:** a *real* chain gap deeper than this is dropped as a phantom parent
    /// and is thereafter *permanently unrecoverable* by this node through the
    /// backfill path — raising the bound moves that cliff, it does not remove it.
    /// The drop is surfaced (never silent): the first one each session records a
    /// CRITICAL `backfill_depth_exceeded`
    /// [`TrustDecision`](super::TrustDecision) naming the lost event, and every
    /// drop bumps [`phantom_depth_dropped`](super::SyncCounters::phantom_depth_dropped).
    pub max_backfill_depth: usize,
    /// Cap on frames in one `Events` response; the requester re-asks for the rest
    /// (`RESPONSE_MAX_FRAMES`).
    pub response_max_frames: usize,
    /// Default `Window.max_count` when a peer asks without one (`CHAT_WINDOW_DEFAULT`).
    pub chat_window_default: u32,
    /// Responder's hard cap on `Window.max_count` (`CHAT_WINDOW_MAX`, PRD §10.7).
    pub chat_window_max: u32,
    /// Catch-up ticks an **unverified** advertised admin tip survives before it is
    /// expired (spec D6 / §13). An [`AdminTip`](super::SyncMessage::AdminTip) is a
    /// peer's claim, not proof; bounding it stops a fabricated higher tip — which
    /// can never be backfilled — from pinning a node fail-closed forever. A real
    /// tip is reconciled (and the suspicion cleared) well within this budget by the
    /// never-windowed membership pull, so only a fabricated tip reaches expiry.
    /// **Not an ordinary tuning knob:** expiry *fails the removal-sensitive access
    /// gate open* — the node stops failing closed on a tip it could not confirm,
    /// so a removal it never saw could leave a removed member trusted as active.
    /// That is a security-relevant transition, surfaced accordingly (never
    /// silent): the first expiry each session records a CRITICAL
    /// `admin_tip_expired` [`TrustDecision`](super::TrustDecision) naming the
    /// abandoned tip, and every expiry bumps
    /// [`suspect_tip_expired`](super::SyncCounters::suspect_tip_expired).
    pub max_unconfirmed_tip_attempts: u32,
    /// Cap on ids in a `WantMembership` `have` **ancestry claim** (#113): the
    /// requester claims a bounded sample of its held set — placed DAG heads, the
    /// most recent causally-placed ids, and a per-tick rotating window over
    /// everything older — instead of enumerating every held id (which exceeded
    /// the 1 MiB frame ceiling near ~30k events). Each claimed id covers its
    /// entire stored ancestry at the responder. The cap bounds how much of the
    /// held set anchors per round: while a claim lands entirely in
    /// responder-unknown territory the responder re-serves already-held events
    /// (bounded duplicate re-serves per tick), and the rotating window
    /// guarantees the claim escapes that state within at most `placed-events`
    /// ticks. Values large enough to overflow a wire frame themselves
    /// (~30k ids) are rejected by [`validate`](Self::validate).
    pub membership_have_max_ids: usize,
    /// Insert retries (one per [`on_tick`](super::SyncEngine::on_tick)) for a
    /// fold-accepted event whose `store.insert` failed (issue #119 — the fold
    /// holds the event Accepted, so dropping it silently would leave the store
    /// and the fold disagreeing for the whole session). When the budget is
    /// exhausted the event is dropped from the retry queue and a CRITICAL
    /// `store_degraded` [`TrustDecision`](super::TrustDecision) is recorded;
    /// peer re-serves remain the healing backstop (#118).
    pub store_retry_attempts: u32,
    /// Cap on fold-accepted events held in memory awaiting insert retry
    /// (issue #119). An arrival beyond the cap is not queued: it is dropped
    /// straight to the CRITICAL `store_degraded` decision, so a store outage
    /// under event flood cannot grow memory unboundedly (Gate-D R4).
    pub max_store_retry_total: usize,
    /// Maximum number of consecutive fold-accepted events persisted in one
    /// `BEGIN IMMEDIATE` `SQLite` transaction (issue #143). Default 32 amortizes
    /// per-transaction overhead while keeping write-lock dwell time short.
    /// `1` is the supported "disable batching" rollback knob: it restores the
    /// pre-#119-era one transaction per accepted event. `0` is invalid (a
    /// batch can never commit). Values above 1024 are rejected to avoid very
    /// long write transactions (the bound mirrors the park/retry caps).
    pub store_insert_batch_size: usize,
    /// Capacity of the in-memory event-id dedup cache checked **before**
    /// signature verification or any store work (issue #143 / #134 §22.2). A
    /// cache hit short-circuits the replay path entirely; correctness still
    /// rests on the store's primary-key idempotency. Default 4096 covers the
    /// MVP room size and recent-sync windows with modest memory.
    /// `0` disables early dedup (the supported rollback knob): a replay then
    /// falls through to full validation and the existing idempotent store
    /// duplicate path. Values above `1_000_000` are rejected to avoid accidental
    /// memory blowups.
    pub early_event_id_dedup_cache_entries: usize,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            // Per-author park cap: set equal to `max_parked_total` on purpose (issue
            // #114). A member returning across a long offline gap must backfill a
            // deep, often single-author, linear chat chain; the whole chain has to
            // sit in the park at once so a bottom-up `wake_park` cascade can accept
            // it. A per-author cap *below* the total cap would evict the middle of
            // that legitimate chain, and its still-parked children would re-request
            // the evicted parents — thrashing the chase to a standstill. Keeping the
            // two equal lets one author use the whole (unchanged) park budget while
            // memory stays bounded by `max_parked_total`.
            max_parked_per_author: 1024,
            max_parked_total: 1024,
            max_backfill_fanout_ids: 256,
            backfill_tokens_per_author: 32,
            backfill_refill_per_tick: 8,
            // Chase depth must exceed a realistic returning-member gap so the by-id
            // backfill can bridge it back to the held set (issue #114). Kept finite
            // and ≤ the park budget, so a phantom-parent chain is still dropped at a
            // hard bound (the Gate-D anti-amplification requirement) — the chase is
            // bounded by this depth, the token bucket, and `max_parked_total`.
            max_backfill_depth: 1024,
            response_max_frames: 512,
            chat_window_default: 200,
            chat_window_max: 1000,
            max_unconfirmed_tip_attempts: 16,
            // 512 ids ≈ 17.4 KiB on the wire — far under the 1 MiB frame cap and
            // deep enough that a node must hold >512 events no peer has seen
            // before any claim coverage degrades (issue #113).
            membership_have_max_ids: 512,
            // Long enough to ride out a transient store fault (a busy_timeout
            // burst, a briefly-full disk) at the MVP's ~1 tick/s cadence, short
            // enough that a genuinely dead store surfaces its CRITICAL
            // `store_degraded` decision within seconds (issue #119).
            store_retry_attempts: 16,
            // Mirrors `max_parked_total`: the same "bounded in-memory frame
            // buffer" shape, sized for the MVP room target (issue #119).
            max_store_retry_total: 1024,
            // Issue #143: amortize SQLite transaction overhead across a
            // consecutive run of accepted events. 32 is a conservative
            // midpoint of the 16/32 range suggested in the issue — short
            // enough to keep the write lock brief under contention, large
            // enough to amortize the BEGIN IMMEDIATE / commit cost on a busy
            // peer burst. See `store_insert_batch_size` field doc for the
            // rollback bounds.
            store_insert_batch_size: 32,
            // Issue #143 / #134 §22.2: the early duplicate-rejection
            // guardrail. 4096 is well above the MVP room target and recent-
            // sync windows; the cache holds raw 32-byte ids, so 4096 ≈ 128
            // KiB plus the FIFO overhead — modest for the guardrail value.
            early_event_id_dedup_cache_entries: 4096,
        }
    }
}

impl SyncConfig {
    /// Validate the bounds are internally consistent (non-zero where a zero would
    /// stall the protocol, and `default <= max` for the chat window).
    ///
    /// # Errors
    /// Returns a stable lowercase reason code if a bound is unusable.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_parked_total == 0 || self.max_parked_per_author == 0 {
            return Err("park_cap_zero");
        }
        if self.max_backfill_fanout_ids == 0 || self.response_max_frames == 0 {
            return Err("fanout_cap_zero");
        }
        if self.chat_window_max == 0 || self.chat_window_default == 0 {
            return Err("chat_window_zero");
        }
        if self.chat_window_default > self.chat_window_max {
            return Err("chat_default_exceeds_max");
        }
        if self.membership_have_max_ids == 0 {
            // A zero-id claim would make every membership pull a full re-serve —
            // permanent duplicate churn on a converged mesh.
            return Err("membership_have_cap_zero");
        }
        if self.membership_have_max_ids > 16_384 {
            // ~34 B/id on the wire: past this the claim itself could approach
            // the 1 MiB frame cap in a big room — the exact request-side stall
            // #113 removes, reintroduced by configuration. 16 384 ids ≈ 557 KiB,
            // comfortably under the cap.
            return Err("membership_have_cap_oversized");
        }
        if self.store_retry_attempts == 0 || self.max_store_retry_total == 0 {
            // Zero would silently disable the #119 insert-failure recovery and
            // reopen the permanent-store-hole path.
            return Err("store_retry_zero");
        }
        if self.store_insert_batch_size == 0 {
            // Zero would never commit a batch — every accepted event would
            // pile up unflushed. `1` is the supported disable-batching knob.
            return Err("store_batch_size_zero");
        }
        if self.store_insert_batch_size > 1024 {
            // Mirrors the park/retry cap shape: a very large batch would hold
            // the SQLite write lock for too long under contention.
            return Err("store_batch_size_oversized");
        }
        if self.early_event_id_dedup_cache_entries > 1_000_000 {
            // Bounded in-memory id store (32 B/id + FIFO overhead) — refuse an
            // accidental memory blowup. `0` is allowed (disables early dedup).
            return Err("early_dedup_cache_oversized");
        }
        Ok(())
    }

    /// Clamp a requested chat window count to `[1, chat_window_max]`, substituting
    /// the default when the requester asks for `0` (spec §6.4).
    #[must_use]
    pub(crate) fn effective_window(&self, requested: u32) -> u32 {
        let n = if requested == 0 {
            self.chat_window_default
        } else {
            requested
        };
        n.min(self.chat_window_max)
    }
}

#[cfg(test)]
mod tests {
    use super::SyncConfig;

    #[test]
    fn default_is_valid() {
        assert_eq!(SyncConfig::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_inconsistent_window() {
        let cfg = SyncConfig {
            chat_window_default: 2000,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("chat_default_exceeds_max"));
    }

    #[test]
    fn rejects_zero_have_cap() {
        let cfg = SyncConfig {
            membership_have_max_ids: 0,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("membership_have_cap_zero"));
    }

    #[test]
    fn rejects_oversized_have_cap() {
        let cfg = SyncConfig {
            membership_have_max_ids: 16_385,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("membership_have_cap_oversized"));
        let max_ok = SyncConfig {
            membership_have_max_ids: 16_384,
            ..SyncConfig::default()
        };
        assert_eq!(max_ok.validate(), Ok(()));
    }

    #[test]
    fn rejects_zero_store_retry_bounds() {
        let cfg = SyncConfig {
            store_retry_attempts: 0,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("store_retry_zero"));
        let cfg = SyncConfig {
            max_store_retry_total: 0,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("store_retry_zero"));
    }

    #[test]
    fn effective_window_clamps_and_defaults() {
        let cfg = SyncConfig::default();
        assert_eq!(cfg.effective_window(0), cfg.chat_window_default);
        assert_eq!(cfg.effective_window(5), 5);
        assert_eq!(cfg.effective_window(9999), cfg.chat_window_max);
    }

    #[test]
    fn rejects_zero_store_batch_size() {
        let cfg = SyncConfig {
            store_insert_batch_size: 0,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("store_batch_size_zero"));
    }

    #[test]
    fn accepts_batch_size_one_as_disable_batching() {
        let cfg = SyncConfig {
            store_insert_batch_size: 1,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn rejects_oversized_store_batch_size() {
        let cfg = SyncConfig {
            store_insert_batch_size: 1025,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("store_batch_size_oversized"));
        let max_ok = SyncConfig {
            store_insert_batch_size: 1024,
            ..SyncConfig::default()
        };
        assert_eq!(max_ok.validate(), Ok(()));
    }

    #[test]
    fn accepts_zero_dedup_cache_as_disabled() {
        let cfg = SyncConfig {
            early_event_id_dedup_cache_entries: 0,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn rejects_oversized_dedup_cache() {
        let cfg = SyncConfig {
            early_event_id_dedup_cache_entries: 1_000_001,
            ..SyncConfig::default()
        };
        assert_eq!(cfg.validate(), Err("early_dedup_cache_oversized"));
        let max_ok = SyncConfig {
            early_event_id_dedup_cache_entries: 1_000_000,
            ..SyncConfig::default()
        };
        assert_eq!(max_ok.validate(), Ok(()));
    }
}
