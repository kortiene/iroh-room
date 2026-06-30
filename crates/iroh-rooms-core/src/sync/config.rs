//! [`SyncConfig`] — the anti-amplification bounds and window defaults
//! (spec `bounded-recent-sync-prototype.md` §4.4).
//!
//! Every bound has a safe default tuned for the MVP target (≤5 peers, full mesh).
//! A Gate-D **NO-GO** condition is an *unbounded* orphan park or backfill, so the
//! engine enforces all of these and logs whenever one drops, evicts, or caps
//! something (spec §4.4 final paragraph / §9).

/// Anti-amplification configuration for the sync engine (spec §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncConfig {
    /// Cap on parked (causally-incomplete) frames **per author key**; oldest is
    /// evicted on overflow (`MAX_PARKED_PER_AUTHOR`).
    pub max_parked_per_author: usize,
    /// Global cap on the orphan park across all authors (`MAX_PARKED_TOTAL`).
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
    /// a phantom and dropped (`MAX_BACKFILL_DEPTH`).
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
    pub max_unconfirmed_tip_attempts: u32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            max_parked_per_author: 64,
            max_parked_total: 1024,
            max_backfill_fanout_ids: 256,
            backfill_tokens_per_author: 32,
            backfill_refill_per_tick: 8,
            max_backfill_depth: 64,
            response_max_frames: 512,
            chat_window_default: 200,
            chat_window_max: 1000,
            max_unconfirmed_tip_attempts: 16,
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
    fn effective_window_clamps_and_defaults() {
        let cfg = SyncConfig::default();
        assert_eq!(cfg.effective_window(0), cfg.chat_window_default);
        assert_eq!(cfg.effective_window(5), 5);
        assert_eq!(cfg.effective_window(9999), cfg.chat_window_max);
    }
}
