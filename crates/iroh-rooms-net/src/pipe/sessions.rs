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
    since_ms: u64,
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

/// A live forwarding session as an owner-side consumer sees it (identity only —
/// no internal teardown key, no QUIC/abort handles). The data source for a
/// per-pipe "connected" indicator (issue #86).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeSessionInfo {
    /// The governing pipe this session forwards for.
    pub pipe_id: [u8; 16],
    /// The QUIC-proven device of the connecting peer. Resolve to an
    /// `IdentityKey` via `Node::snapshot()` if a human identity is needed.
    pub device: EndpointId,
    /// Connected-at wall-clock ms (advisory; owner's clock). `0` if the clock
    /// read failed at registration.
    pub since_ms: u64,
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
                    since_ms: super::now_ms(),
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

    /// Count of live sessions for one pipe (issue #86). Filters the live table —
    /// always exactly the sessions currently forwarding for `pipe_id`, so it is
    /// decrement-correct with every teardown/deregister path (no separate counter).
    #[must_use]
    pub fn count_for(&self, pipe_id: &[u8; 16]) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|e| &e.pipe_id == pipe_id)
            .count()
    }

    /// Per-session identity across all pipes (issue #86) — the Pipes-panel source.
    #[must_use]
    pub fn info(&self) -> Vec<PipeSessionInfo> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|e| PipeSessionInfo {
                pipe_id: e.pipe_id,
                device: e.device,
                since_ms: e.since_ms,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{PipeSessionInfo, PipeSessions};
    use iroh::{EndpointId, SecretKey};

    /// A valid, deterministic `EndpointId` for `seed` (the crate-wide unit-test
    /// idiom — mirrors `pipe::audit`'s tests).
    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

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

    // --- issue #86: per-pipe live-session accessors ---
    //
    // The populated / teardown-decrement paths need a real QUIC `Connection` to
    // build a `SessionEntry`, so they live in the loopback e2e (`pipe_e2e.rs`,
    // spec §7.2). These pure tests pin the accessors' empty-table contract and the
    // public `PipeSessionInfo` shape — the parts reachable without a network.

    #[test]
    fn count_for_empty_table_is_zero_for_any_pipe_id() {
        let s = PipeSessions::new();
        assert_eq!(s.count_for(&[0u8; 16]), 0);
        assert_eq!(s.count_for(&[0xab; 16]), 0);
        assert_eq!(s.count_for(&[7u8; 16]), 0);
    }

    #[test]
    fn info_on_empty_table_is_empty() {
        let s = PipeSessions::new();
        assert!(s.info().is_empty());
    }

    #[test]
    fn teardown_pipe_with_no_live_session_is_an_empty_noop() {
        // `Node::pipe_close` tears down every live session for the pipe via
        // `teardown_pipe`. Closing a pipe that was exposed but never connected to
        // (no session ever registered) is a real path — it must be a safe no-op:
        // nothing torn down, no panic, and the per-pipe/node-wide views still read
        // zero afterwards (teardown can only ever decrement).
        let s = PipeSessions::new();
        assert!(
            s.teardown_pipe(&[0xcd; 16]).is_empty(),
            "no live session for the pipe ⇒ nothing torn down"
        );
        assert_eq!(s.count_for(&[0xcd; 16]), 0);
        assert!(s.info().is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn node_wide_and_per_pipe_views_agree_a_fresh_table_is_empty() {
        // The node-wide count (`len`, unchanged by #86 — AC4) and the new per-pipe
        // count (`count_for`) must never disagree about "zero sessions", and the
        // Pipes-panel row source (`info`) filtered by a pipe must equal that pipe's
        // `count_for` (§6.4 — both accessors read the one table). Pinned at the
        // reachable empty scope; the populated case is proven in the loopback e2e.
        let s = PipeSessions::new();
        assert_eq!(s.len(), 0, "node-wide total");
        for pipe in [[0u8; 16], [1u8; 16], [0xff; 16]] {
            assert_eq!(s.count_for(&pipe), 0, "per-pipe count for a fresh table");
            assert_eq!(
                s.info().iter().filter(|i| i.pipe_id == pipe).count(),
                s.count_for(&pipe),
                "info() filtered by pipe must equal count_for(pipe)"
            );
        }
    }

    #[test]
    fn pipe_session_info_is_copy_and_compares_by_every_field() {
        // A consumer (the Pipes panel) dedups/compares rows by value and passes
        // them around by copy — pin `Copy` + structural `Eq` over all three fields
        // so dropping a derive or renaming a field is a compile/test failure here.
        let dev = device(7);
        let base = PipeSessionInfo {
            pipe_id: [1u8; 16],
            device: dev,
            since_ms: 42,
        };

        // `Copy`: `base` stays usable after being read into `copy`.
        let copy = base;
        assert_eq!(base, copy);
        assert_eq!(base.pipe_id, [1u8; 16]);
        assert_eq!(base.device, dev);
        assert_eq!(base.since_ms, 42);

        // A difference in any single field breaks equality.
        assert_ne!(
            base,
            PipeSessionInfo {
                pipe_id: [2u8; 16],
                ..base
            },
            "distinct pipe_id must not compare equal"
        );
        assert_ne!(
            base,
            PipeSessionInfo {
                device: device(8),
                ..base
            },
            "distinct device must not compare equal"
        );
        assert_ne!(
            base,
            PipeSessionInfo {
                since_ms: 43,
                ..base
            },
            "distinct since_ms must not compare equal"
        );

        // `Debug` is used in panel/log lines — it must render something.
        assert!(!format!("{base:?}").is_empty());
    }
}
