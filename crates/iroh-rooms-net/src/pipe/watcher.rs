//! Tear-down-on-learn: the owner's live-session revocation watcher (spec §4.5/D5;
//! `PHASE-0-SPIKE.md` Membership §5).
//!
//! A single per-owner task re-evaluates **every** live session on each anti-entropy
//! tick: it recomputes the composed [`gate::evaluate`] against the **current**
//! snapshot + pipe status, and severs any session that no longer passes — auditing
//! `pipe.torndown:<cause>`. Poll-based teardown is the simplest correct
//! implementation, and its latency bound (≤ one tick after the owner *learns* of the
//! change) is exactly the §5 / Residual-#2 guarantee ("bounded by reachability, then
//! immediate"). A push-based membership-change broadcast is a noted refinement (OQ-3).

use std::sync::Arc;
use std::time::Duration;

use iroh_rooms_core::event::keys::DeviceKey;
use tokio::time::MissedTickBehavior;

use super::audit::PipeAuditSink;
use super::gate::{self, PipeGateVerdict};
use super::registry::PipeRegistry;
use super::runtime::PipeQuery;
use super::sessions::PipeSessions;
use crate::pipe::now_ms;

/// Run the teardown watcher until the task is aborted (Node shutdown).
pub(crate) async fn watch(
    query: PipeQuery,
    registry: Arc<PipeRegistry>,
    sessions: Arc<PipeSessions>,
    audit: Arc<dyn PipeAuditSink>,
    tick: Duration,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let live = sessions.live();
        if live.is_empty() {
            continue;
        }
        let now = now_ms();
        for s in live {
            let device = DeviceKey::from_bytes(*s.device.as_bytes());
            if let PipeGateVerdict::Reject(cause) =
                gate::evaluate(&query, &registry, &device, s.pipe_id, now).await
            {
                // Sever it (abort splice + close the connection with the teardown
                // code) and audit the revocation.
                if let Some((dev, pid)) = sessions.teardown(s.id) {
                    audit.torndown(dev, &pid, cause);
                }
            }
        }
    }
}
