//! The single wall-clock read shared by the commands that stamp a `created_at`
//! into an event (`identity create`, `room create`).
//!
//! `created_at` is advisory/display-only in the protocol (never used for ordering
//! or authorization — Membership §2.3), so a saturating epoch read is exactly
//! right: it never panics and never needs sub-millisecond precision.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (saturating; `0` if the clock predates it,
/// `u64::MAX` if it somehow overflows a `u64` of milliseconds).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
