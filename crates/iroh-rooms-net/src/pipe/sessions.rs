//! [`PipeSessions`] — the owner's table of live forwarded sessions (spec §4.5/D5).
//!
//! Each accepted+gated bidi stream registers a session keyed by a monotonic id and
//! carrying the connecting `device`, the governing `pipe_id`, the QUIC
//! [`Connection`] handle, and the splice task's [`AbortHandle`]. The teardown
//! watcher (spec §6.5 `watcher.rs`) scans this table each tick and severs any
//! session whose authorization no longer holds; an owner `close` severs every
//! session for a `pipe_id`. Tearing a session down aborts its splice task **and**
//! closes the QUIC connection with [`PIPE_TEARDOWN_CODE`](super::alpn::PIPE_TEARDOWN_CODE),
//! so the connector observes the drop and stops forwarding.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use iroh::endpoint::Connection;
use iroh::EndpointId;
use tokio::task::AbortHandle;

use super::alpn::PIPE_TEARDOWN_CODE;

/// One live forwarded session (one bidi stream spliced to a loopback target).
struct SessionEntry {
    device: EndpointId,
    pipe_id: [u8; 16],
    conn: Connection,
    abort: AbortHandle,
}

/// A live session as the watcher sees it (no handles — just identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSession {
    /// The internal session id (teardown key).
    pub id: u64,
    /// The connecting device.
    pub device: EndpointId,
    /// The governing pipe.
    pub pipe_id: [u8; 16],
}

/// The owner's live-session table.
#[derive(Default)]
pub struct PipeSessions {
    next: AtomicU64,
    sessions: Mutex<HashMap<u64, SessionEntry>>,
}

impl std::fmt::Debug for PipeSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.sessions.lock().map(|g| g.len()).unwrap_or_default();
        write!(f, "PipeSessions {{ live: {n} }}")
    }
}

impl PipeSessions {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a live session; returns its teardown id.
    pub fn register(
        &self,
        device: EndpointId,
        pipe_id: [u8; 16],
        conn: Connection,
        abort: AbortHandle,
    ) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                SessionEntry {
                    device,
                    pipe_id,
                    conn,
                    abort,
                },
            );
        id
    }

    /// Drop a session's bookkeeping (called when its splice task finishes on its
    /// own). Does **not** abort/close — the task already ended.
    pub fn deregister(&self, id: u64) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
    }

    /// A snapshot of the live sessions for the watcher to re-evaluate.
    #[must_use]
    pub fn live(&self) -> Vec<LiveSession> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(&id, e)| LiveSession {
                id,
                device: e.device,
                pipe_id: e.pipe_id,
            })
            .collect()
    }

    /// Sever one session: abort its splice task and close the QUIC connection with
    /// the stable teardown code. Returns the `(device, pipe_id)` torn down, if it
    /// was still live (so the caller can audit it).
    pub fn teardown(&self, id: u64) -> Option<(EndpointId, [u8; 16])> {
        let entry = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)?;
        entry.abort.abort();
        entry.conn.close(PIPE_TEARDOWN_CODE, b"pipe-revoked");
        Some((entry.device, entry.pipe_id))
    }

    /// Sever every live session for `pipe_id` (an owner `close` / owner exit).
    /// Returns the torn-down `(device, pipe_id)` pairs for auditing.
    pub fn teardown_pipe(&self, pipe_id: &[u8; 16]) -> Vec<(EndpointId, [u8; 16])> {
        let ids: Vec<u64> = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, e)| &e.pipe_id == pipe_id)
            .map(|(&id, _)| id)
            .collect();
        ids.into_iter().filter_map(|id| self.teardown(id)).collect()
    }

    /// The number of live sessions (tests / observability).
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether there are no live sessions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::PipeSessions;

    #[test]
    fn fresh_table_is_empty() {
        let s = PipeSessions::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.live().is_empty());
    }

    #[test]
    fn deregister_unknown_id_is_a_noop() {
        let s = PipeSessions::new();
        s.deregister(42); // no panic
        assert!(s.is_empty());
    }

    #[test]
    fn teardown_unknown_id_returns_none() {
        let s = PipeSessions::new();
        assert_eq!(s.teardown(7), None);
    }
}
