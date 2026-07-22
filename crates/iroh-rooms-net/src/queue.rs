//! Byte-bounded priority queues for the v1 event-plane transport (issue #141,
//! `#134 §12.3`).
//!
//! Tokio's `mpsc` capacity is **item-count** based, so it cannot bound queued
//! frame **bytes** directly. This module provides a small, dependency-free
//! `Mutex<QueueState> + Notify` queue that:
//!
//! - charges `body.len()` for every frame (spec D1 — encoded body bytes only,
//!   never the length prefix, frame count, or allocator overhead);
//! - enforces two coupled caps: a per-peer byte cap and a (logical) per-stream
//!   byte cap (spec D4 — for v1 there is exactly one event stream per peer);
//! - carries four internal priority classes — `Governance > Checkpoint >
//!   Content > BlobHints` — so a governance frame can skip ahead of an already
//!   saturated content backlog (spec D3 / D5); and
//! - preserves the existing recovery shape on true budget exhaustion: the caller
//!   audits `transport.queue.saturated` and closes the peer link (spec §7).
//!
//! The queue never decodes `SyncMessage` payloads (room events, capability
//! secrets, blob bytes); classification reads only the message **variant**, so
//! audit/classification paths cannot leak frame contents (spec §9 / D7).
//!
//! ## Producer / consumer split
//!
//! [`BytePriorityQueue::channel`] returns a cloneable producer handle
//! ([`BytePriorityQueue`]) and a single-owner consumer handle
//! ([`BytePriorityReceiver`]). The producer is cloned into every reader/route
//! path; the consumer is owned by either the per-peer writer task (outbound) or
//! the engine driver pump (inbound). Closing the queue (or dropping the
//! receiver) wakes any waiting consumer and lets it drain remaining frames
//! before observing `None`, mirroring the prior `mpsc::Sender`-drop contract.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};

use iroh_rooms_core::sync::{PeerId, SyncMessage};
use tokio::sync::Notify;

/// Default per-peer queued-byte cap for both inbound and outbound event-plane
/// queues (`#134 §12.3` — 8 MiB queued per peer).
pub(crate) const DEFAULT_PER_PEER_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// Default per-subscribed-stream queued-byte cap (`#134 §12.3` — 2 MiB per
/// subscribed stream per peer). For v1 each peer has exactly one logical event
/// ALPN stream, so this is the per-peer content/stream bucket (spec D4).
pub(crate) const DEFAULT_PER_STREAM_QUEUE_BYTES: usize = 2 * 1024 * 1024;

/// The four scheduler priorities (spec D3). `Ord` and the `as u8` discriminant
/// both put `Governance` highest; the consumer drains larger ordinals first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum QueuePriority {
    /// Lowest: reconciliation hints / `WantEvents` / `NotFound` / undecodable.
    BlobHints = 0,
    /// Publication / subscription (`Events`, `WantRecentChat`).
    Content = 1,
    /// Checkpoint + session control (`Heads`, `ProveCapability`).
    Checkpoint = 2,
    /// Highest: `AdminTip`, `WantMembership`.
    Governance = 3,
}

impl QueuePriority {
    /// Iteration order from highest priority to lowest — the order the consumer
    /// drains. Kept explicit (not derived from `Ord`) so a future reshuffle is
    /// a one-line edit with a clear semantic.
    const DRAIN_ORDER: [Self; 4] = [
        Self::Governance,
        Self::Checkpoint,
        Self::Content,
        Self::BlobHints,
    ];

    const fn ordinal(self) -> usize {
        self as u8 as usize
    }
}

/// The `#134 §12.2` message family. The transport maps existing v1
/// [`SyncMessage`] variants onto these families via
/// [`classify_sync_message`] (spec D3). The family decides both the queue
/// priority and whether the frame charges the per-stream byte bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueFamily {
    Session,
    Governance,
    Subscription,
    Publication,
    Reconciliation,
    Checkpoint,
    BlobHints,
}

impl QueueFamily {
    /// Scheduler priority for this family (spec D3 mapping).
    #[must_use]
    pub(crate) const fn priority(self) -> QueuePriority {
        match self {
            Self::Governance => QueuePriority::Governance,
            // `ProveCapability` is session control and must not be starved
            // behind content during join bootstrap (spec D3 rationale).
            Self::Checkpoint | Self::Session => QueuePriority::Checkpoint,
            Self::Publication | Self::Subscription => QueuePriority::Content,
            Self::Reconciliation | Self::BlobHints => QueuePriority::BlobHints,
        }
    }

    /// Whether this family charges the per-stream content/subpublication
    /// bucket as well as the per-peer bucket (spec D4). Governance,
    /// checkpoint, and session control charge **only** the per-peer cap, so a
    /// saturated content stream still leaves room for `AdminTip` /
    /// `WantMembership` / `Heads` / `ProveCapability`.
    #[must_use]
    pub(crate) const fn charges_stream_budget(self) -> bool {
        matches!(
            self,
            Self::Publication | Self::Subscription | Self::Reconciliation | Self::BlobHints
        )
    }
}

/// Map an outbound v1 [`SyncMessage`] to its transport queue family (spec D3).
///
/// This reads only the variant tag, never any field bytes — it is safe to call
/// on a frame the engine has not yet validated and never logs message bodies.
/// The catch-all arm future-proofs against `#[non_exhaustive]` variants added
/// to `SyncMessage` by another crate: an unknown variant falls to `BlobHints`
/// (lowest priority) rather than failing to enqueue.
#[must_use]
pub(crate) fn classify_sync_message(msg: &SyncMessage) -> QueueFamily {
    match msg {
        SyncMessage::AdminTip { .. } | SyncMessage::WantMembership { .. } => {
            QueueFamily::Governance
        }
        SyncMessage::Heads { .. } => QueueFamily::Checkpoint,
        SyncMessage::ProveCapability { .. } => QueueFamily::Session,
        SyncMessage::Events { .. } => QueueFamily::Publication,
        SyncMessage::WantRecentChat { .. } => QueueFamily::Subscription,
        SyncMessage::WantEvents { .. } | SyncMessage::NotFound { .. } => {
            QueueFamily::Reconciliation
        }
        // `SyncMessage` is `#[non_exhaustive]` and lives in another crate; an
        // unknown variant is lowest-priority (still bounded, still delivered).
        _ => QueueFamily::BlobHints,
    }
}

/// Map an inbound raw byte frame to a queue family by attempting to decode it
/// as a [`SyncMessage`] (spec §5 step 2). An undecodable body falls back to
/// [`QueueFamily::BlobHints`] (lowest priority) and is still bounded by the
/// byte budget; the engine pump will decode again and log the drop as today.
#[must_use]
pub(crate) fn classify_inbound_bytes(bytes: &[u8]) -> QueueFamily {
    match SyncMessage::decode(bytes) {
        Ok(msg) => classify_sync_message(&msg),
        Err(_) => QueueFamily::BlobHints,
    }
}

/// Why a `try_push` rejected a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushError {
    /// The relevant byte cap (per-peer and/or per-stream) has insufficient
    /// headroom. The caller preserves the existing recovery shape: audit
    /// `transport.queue.saturated` and close the peer link (spec §7).
    Saturated,
    /// The queue is closed (the consumer went away — pump shutdown / peer
    /// unregister). The caller drops the frame silently (the receiver is gone).
    Closed,
}

/// One queued frame: the producer-side metadata the consumer needs to deliver
/// it (the engine pump for inbound, the writer task for outbound). The body is
/// the verbatim encoded [`SyncMessage`]; `bytes` is cached `body.len()` so the
/// accounting refund on pop never re-reads the allocation.
pub(crate) struct QueuedFrame {
    pub peer: PeerId,
    pub body: Vec<u8>,
    pub bytes: usize,
    pub family: QueueFamily,
}

/// Per-peer accounting: total queued body bytes plus the bytes charging the
/// logical stream bucket (content/reconciliation/blob-hints only — spec D4).
#[derive(Default)]
struct PeerBudget {
    total: usize,
    stream: usize,
}

struct QueueState {
    closed: bool,
    peer_cap: usize,
    stream_cap: usize,
    budgets: HashMap<PeerId, PeerBudget>,
    /// One deque per priority ordinal; index by `QueuePriority::ordinal`.
    by_priority: [VecDeque<QueuedFrame>; 4],
}

impl QueueState {
    fn new(peer_cap: usize, stream_cap: usize) -> Self {
        Self {
            closed: false,
            peer_cap,
            stream_cap,
            budgets: HashMap::new(),
            // `[T; N]::default()` is implemented for N ≤ 32 via std; this yields
            // four independent empty deques (Default does not require Copy).
            by_priority: Default::default(),
        }
    }

    fn total_depth(&self) -> usize {
        self.budgets.values().map(|b| b.total).sum()
    }

    fn pop_highest(&mut self) -> Option<QueuedFrame> {
        for priority in QueuePriority::DRAIN_ORDER {
            if let Some(frame) = self.by_priority[priority.ordinal()].pop_front() {
                if let Some(budget) = self.budgets.get_mut(&frame.peer) {
                    budget.total = budget.total.saturating_sub(frame.bytes);
                    if frame.family.charges_stream_budget() {
                        budget.stream = budget.stream.saturating_sub(frame.bytes);
                    }
                    if budget.total == 0 {
                        self.budgets.remove(&frame.peer);
                    }
                }
                return Some(frame);
            }
        }
        None
    }
}

/// The producer side of a byte-priority queue. Cheap to clone (one `Arc`);
/// clones share the same underlying state. Both the inbound sink (one
/// multi-peer queue) and the per-peer outbound queues use this type.
#[derive(Clone)]
pub(crate) struct BytePriorityQueue {
    state: Arc<Mutex<QueueState>>,
    notify: Arc<Notify>,
}

/// The single-owner consumer side of a byte-priority queue.
pub(crate) struct BytePriorityReceiver {
    state: Arc<Mutex<QueueState>>,
    notify: Arc<Notify>,
}

impl BytePriorityQueue {
    /// Construct a producer/consumer pair with the given per-peer and
    /// per-stream byte caps. Both caps are clamped to at least `1` by callers
    /// (a zero cap would reject every frame, including governance/session
    /// control during shutdown).
    pub(crate) fn channel(peer_cap: usize, stream_cap: usize) -> (Self, BytePriorityReceiver) {
        let state = Arc::new(Mutex::new(QueueState::new(peer_cap, stream_cap)));
        let notify = Arc::new(Notify::new());
        (
            Self {
                state: state.clone(),
                notify: notify.clone(),
            },
            BytePriorityReceiver { state, notify },
        )
    }

    /// Try to enqueue `body` from `peer` with classification `family`.
    /// Non-blocking. Charges `body.len()` against the peer cap, and against
    /// the stream cap when [`QueueFamily::charges_stream_budget`].
    pub(crate) fn try_push(
        &self,
        peer: PeerId,
        body: Vec<u8>,
        family: QueueFamily,
    ) -> Result<(), PushError> {
        let bytes = body.len();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(PushError::Closed);
        }
        // Snapshot the caps before the mutable budget borrow so the borrow
        // checker can see the immutable `state` read ends before `entry(...)` overrides it.
        let peer_cap = state.peer_cap;
        let stream_cap = state.stream_cap;
        let charges_stream = family.charges_stream_budget();
        let budget = state.budgets.entry(peer).or_default();
        let new_total = budget.total.saturating_add(bytes);
        if new_total > peer_cap {
            return Err(PushError::Saturated);
        }
        if charges_stream {
            let new_stream = budget.stream.saturating_add(bytes);
            if new_stream > stream_cap {
                return Err(PushError::Saturated);
            }
            budget.stream = new_stream;
        }
        budget.total = new_total;
        let ord = family.priority().ordinal();
        state.by_priority[ord].push_back(QueuedFrame {
            peer,
            body,
            bytes,
            family,
        });
        drop(state);
        // New frame available: wake one waiting consumer (or none if no one is
        // waiting — the next recv will pop it directly).
        self.notify.notify_one();
        Ok(())
    }

    /// Current queued body bytes summed across all peers. `OutboundQueue::depth`
    /// and the verbose `outbound_depth=<N>` CLI diagnostic both report this —
    /// its unit changed from frames to bytes in #141.
    pub(crate) fn depth_bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_depth()
    }

    /// Mark the queue closed. Future `try_push` calls return [`PushError::Closed`];
    /// consumers drain remaining frames and then observe `None` from `recv`.
    /// Idempotent; calling on an already-closed queue is a no-op. Wakes any
    /// waiting consumer so it can observe the close (and drain or exit).
    pub(crate) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        drop(state);
        // `notify_waiters`, not `notify_one`: a close must wake *every* waiting
        // consumer (currently there is at most one, but wake-all is the safe
        // superset and matches `Notify`'s "broadcast to current waiters" mode).
        self.notify.notify_waiters();
    }
}

impl BytePriorityReceiver {
    /// Pop the next frame in priority order without waiting. Returns `None`
    /// when the queue is empty (regardless of close state). Test-helper only:
    /// production consumers (`writer_task`, the engine pump) use the async
    /// [`Self::recv`].
    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Option<QueuedFrame> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_highest()
    }

    /// Await the next frame in priority order. Returns `None` once the queue is
    /// closed **and** fully drained (mirrors the prior `mpsc::Sender`-drop
    /// contract: closing lets in-flight frames complete).
    pub(crate) fn recv(&mut self) -> impl Future<Output = Option<QueuedFrame>> + '_ {
        let state = self.state.clone();
        let notify = self.notify.clone();
        async move {
            loop {
                if let Some(frame) = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_highest()
                {
                    return Some(frame);
                }
                // Enroll as a waiter *before* re-checking closed/drained state.
                // `Notified::enable()` registers this future in `Notify`'s waiter
                // list up front, so a `notify_waiters()` from a concurrent
                // `close()` that lands after this check is captured by the
                // pending `.await` below instead of being lost. Without this
                // enrolment there is a lost-wakeup window: close() could set
                // `closed=true` and fire `notify_waiters()` between our snapshot
                // read and our registration, leaving the consumer blocked
                // forever on an already-closed, drained queue.
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let snapshot_closed = {
                    let s = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    s.closed && s.total_depth() == 0
                };
                if snapshot_closed {
                    return None;
                }
                // Wait for either a push (notify_one) or a close (notify_waiters).
                // `Notify::notified()` is cancel-safe: a notified permit is
                // consumed before the next await, so a spurious wake just loops.
                notified.await;
            }
        }
    }
}

impl Drop for BytePriorityReceiver {
    fn drop(&mut self) {
        // Mirror `mpsc::Receiver`'s drop: a dropped consumer means future pushes
        // cannot be drained, so mark the queue closed and let producers observe
        // `PushError::Closed`. Without this, a pump shutdown with live accept
        // tasks would silently accumulate unread frames against the budget.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_rooms_core::event::ids::{EventId, RoomId};
    use iroh_rooms_core::sync::{SyncMessage, Window};

    fn peer(b: u8) -> PeerId {
        PeerId::from_bytes([b; 32])
    }

    fn other_peer(b: u8) -> PeerId {
        PeerId::from_bytes([b ^ 0xFF; 32])
    }

    fn admin_tip() -> Vec<u8> {
        SyncMessage::AdminTip {
            room_id: RoomId::from_bytes([0x11; 32]),
            tip: None,
        }
        .encode()
    }

    fn heads() -> Vec<u8> {
        SyncMessage::Heads {
            room_id: RoomId::from_bytes([0x11; 32]),
            heads: vec![EventId::from_bytes([0xaa; 32])],
        }
        .encode()
    }

    fn events(payload_len: usize) -> Vec<u8> {
        SyncMessage::Events {
            room_id: RoomId::from_bytes([0x11; 32]),
            frames: vec![vec![0xEE; payload_len]],
        }
        .encode()
    }

    fn want_events() -> Vec<u8> {
        SyncMessage::WantEvents {
            room_id: RoomId::from_bytes([0x11; 32]),
            ids: vec![EventId::from_bytes([0x33; 32])],
        }
        .encode()
    }

    // --- D1: depth is bytes, not frames -------------------------------------

    #[test]
    fn depth_is_bytes_not_frames() {
        let (tx, mut rx) = BytePriorityQueue::channel(1024, 1024);
        let p = peer(0x01);
        assert_eq!(tx.depth_bytes(), 0);

        // Three bodies of distinct lengths all charge their `body.len()`.
        tx.try_push(p, vec![0u8; 10], QueueFamily::Checkpoint)
            .expect("push 10B checkpoint");
        tx.try_push(p, vec![0u8; 20], QueueFamily::Checkpoint)
            .expect("push 20B checkpoint");
        tx.try_push(p, vec![0u8; 30], QueueFamily::Checkpoint)
            .expect("push 30B checkpoint");
        assert_eq!(tx.depth_bytes(), 60);

        // Popping one frame refunds exactly its body length.
        let popped = rx.try_recv().expect("one frame is queued");
        assert_eq!(popped.bytes, 10);
        assert_eq!(tx.depth_bytes(), 50);
    }

    // --- Exact per-peer cap -------------------------------------------------

    #[test]
    fn exact_per_peer_cap_admits_and_then_rejects() {
        let (tx, _rx) = BytePriorityQueue::channel(100, 100);
        let p = peer(0x02);
        // Fill the per-peer cap exactly with checkpoint frames (no stream charge).
        let mut filled = 0;
        while filled + 10 <= 100 {
            tx.try_push(p, vec![0u8; 10], QueueFamily::Checkpoint)
                .expect("fits the peer cap");
            filled += 10;
        }
        assert_eq!(filled, 100);
        assert_eq!(tx.depth_bytes(), 100);
        // One more byte must be rejected with Saturated.
        assert_eq!(
            tx.try_push(p, vec![0u8; 1], QueueFamily::Checkpoint),
            Err(PushError::Saturated)
        );
    }

    // --- Exact per-stream cap: content rejects even with peer headroom ------

    #[test]
    fn per_stream_cap_rejects_content_with_peer_headroom() {
        // Peer cap 200, stream cap 50: content charges BOTH.
        let (tx, _rx) = BytePriorityQueue::channel(200, 50);
        let p = peer(0x03);
        // Fill the stream bucket exactly.
        let mut filled = 0;
        while filled + 10 <= 50 {
            tx.try_push(p, vec![0u8; 10], QueueFamily::Publication)
                .expect("fits the stream cap");
            filled += 10;
        }
        assert_eq!(filled, 50);
        // The peer cap still has 150 bytes of headroom, but content must be
        // rejected because the stream bucket is full.
        assert_eq!(
            tx.try_push(p, vec![0u8; 10], QueueFamily::Publication),
            Err(PushError::Saturated)
        );
    }

    // --- D4/D5: governance skips a saturated content stream -----------------

    #[test]
    fn governance_lands_and_drains_before_saturated_content() {
        // Stream cap 50, peer cap 200. Fill the stream bucket with content
        // (the test's analog of a saturated content backlog), then push a
        // governance frame. It must land despite the saturated stream bucket
        // AND must pop BEFORE the older content (skip-ahead, not eviction).
        let (tx, mut rx) = BytePriorityQueue::channel(200, 50);
        let p = peer(0x04);
        let mut filled = 0;
        while filled + 10 <= 50 {
            tx.try_push(p, vec![0u8; 10], QueueFamily::Publication)
                .expect("content fills the stream cap");
            filled += 10;
        }
        // The acceptance-critical assertion: AdminTip lands after content
        // saturated the stream bucket — governance charges peer-only.
        let admin_tip_body = admin_tip();
        tx.try_push(p, admin_tip_body.clone(), QueueFamily::Governance)
            .expect("governance bypasses the saturated stream bucket");
        // WantMembership is the other acceptance-critical governance variant.
        let want_mem = SyncMessage::WantMembership {
            room_id: RoomId::from_bytes([0x11; 32]),
            have: vec![],
        }
        .encode();
        tx.try_push(p, want_mem.clone(), QueueFamily::Governance)
            .expect("WantMembership bypasses the saturated stream bucket");

        // Drain order is governance first.
        let first = rx.try_recv().expect("first pop");
        assert_eq!(
            first.family,
            QueueFamily::Governance,
            "the first drained frame must be governance (skip-ahead)"
        );
        assert_eq!(first.body, admin_tip_body);
        let second = rx.try_recv().expect("second pop");
        assert_eq!(second.family, QueueFamily::Governance);
        assert_eq!(second.body, want_mem);
        // Now content begins to drain.
        let third = rx.try_recv().expect("third pop");
        assert_eq!(third.family, QueueFamily::Publication);
    }

    // --- D3: priority ordering ----------------------------------------------

    #[test]
    fn pop_order_is_governance_checkpoint_content_blob_hints() {
        let (tx, mut rx) = BytePriorityQueue::channel(8 * 1024, 8 * 1024);
        let p = peer(0x05);
        // Push one frame per family, low-to-high priority, all unique bodies.
        let want_events_body = want_events(); // BlobHints
        let events_body = events(8); // Content (Publication)
        let heads_body = heads(); // Checkpoint
        let admin_tip_body = admin_tip(); // Governance

        tx.try_push(p, want_events_body.clone(), QueueFamily::Reconciliation)
            .expect("push blob-hints");
        tx.try_push(p, events_body.clone(), QueueFamily::Publication)
            .expect("push content");
        tx.try_push(p, heads_body.clone(), QueueFamily::Checkpoint)
            .expect("push checkpoint");
        tx.try_push(p, admin_tip_body.clone(), QueueFamily::Governance)
            .expect("push governance");

        // Drain order: governance, checkpoint, content, blob-hints.
        let o1 = rx.try_recv().expect("pop 1");
        assert_eq!(o1.family, QueueFamily::Governance);
        assert_eq!(o1.body, admin_tip_body);
        let o2 = rx.try_recv().expect("pop 2");
        assert_eq!(o2.family, QueueFamily::Checkpoint);
        assert_eq!(o2.body, heads_body);
        let o3 = rx.try_recv().expect("pop 3");
        assert_eq!(o3.family, QueueFamily::Publication);
        assert_eq!(o3.body, events_body);
        let o4 = rx.try_recv().expect("pop 4");
        assert_eq!(o4.family, QueueFamily::Reconciliation);
        assert_eq!(o4.body, want_events_body);
        assert!(rx.try_recv().is_none(), "queue is drained");
    }

    // --- §6.1.6: malformed inbound body is bounded --------------------------

    #[test]
    fn undecodable_inbound_is_classified_and_bounded_as_blob_hints() {
        // An undecodable body must still be bounded by the byte budget; it
        // classifies as BlobHints and charges the stream bucket (lowest
        // priority). The engine pump re-decodes and drops it as today.
        let undecodable = vec![0xff, 0x00, 0x42];
        assert_eq!(classify_inbound_bytes(&undecodable), QueueFamily::BlobHints);
        assert!(QueueFamily::BlobHints.charges_stream_budget());

        let (tx, _rx) = BytePriorityQueue::channel(2, 2);
        let p = peer(0x06);
        // The 3-byte body exceeds the 2-byte stream cap — must reject.
        assert_eq!(
            tx.try_push(p, undecodable, QueueFamily::BlobHints),
            Err(PushError::Saturated)
        );
        assert_eq!(tx.depth_bytes(), 0);
    }

    // --- D4: per-peer vs per-stream independence ----------------------------

    #[test]
    fn peer_cap_separates_two_peers() {
        // Two peers each get their own per-peer bucket; one peer saturating
        // its cap must not block the other.
        let (tx, _rx) = BytePriorityQueue::channel(20, 20);
        let a = peer(0x07);
        let b = other_peer(0x07);
        tx.try_push(a, vec![0u8; 20], QueueFamily::Checkpoint)
            .expect("peer a fills its own cap");
        assert_eq!(
            tx.try_push(a, vec![0u8; 1], QueueFamily::Checkpoint),
            Err(PushError::Saturated),
            "peer a is full"
        );
        tx.try_push(b, vec![0u8; 10], QueueFamily::Checkpoint)
            .expect("peer b has its own cap");
    }

    // --- Async consumer: enqueue-before-wait and close-drains-then-none -----

    #[tokio::test]
    async fn recv_returns_pushed_frame_then_none_after_close_drain() {
        let (tx, mut rx) = BytePriorityQueue::channel(1024, 1024);
        let p = peer(0x08);
        tx.try_push(p, vec![0u8; 5], QueueFamily::Checkpoint)
            .expect("push before close");

        // Close while a frame is buffered: the consumer must still drain it,
        // then observe `None`.
        tx.close();

        let first = rx.recv().await.expect("buffered frame drains after close");
        assert_eq!(first.bytes, 5);
        let second = rx.recv().await;
        assert!(
            second.is_none(),
            "after close + drain, recv must yield None"
        );
    }

    #[tokio::test]
    async fn recv_wakes_on_close_while_parked_on_empty_open_queue() {
        // Regression for the lost-wakeup race: a consumer parked in
        // `recv().await` on an empty, still-open queue must observe `None`
        // when another task calls `close()`. `recv` enrolls as a `Notify`
        // waiter *before* re-checking closed/drained state, so the
        // `notify_waiters()` from `close()` is captured even if it fires
        // after the state snapshot. Without the enrolment this hangs forever.
        let (tx, mut rx) = BytePriorityQueue::channel(1024, 1024);
        tokio::spawn(async move {
            // Yield first so the consumer is parked in `recv().await` on an
            // empty open queue before we close it.
            tokio::task::yield_now().await;
            tx.close();
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("recv must not hang after a concurrent close");
        assert!(
            result.is_none(),
            "recv parked on an empty open queue must return None after close"
        );
    }

    #[tokio::test]
    async fn recv_wakes_on_push() {
        // wait-before-enqueue: the consumer is waiting when a producer pushes.
        let (tx, mut rx) = BytePriorityQueue::channel(1024, 1024);
        let tx2 = tx.clone();
        let p = peer(0x09);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            tx2.try_push(p, vec![0u8; 7], QueueFamily::Governance)
                .expect("push wakes the waiter");
        });
        let frame = rx.recv().await.expect("woken by the push");
        assert_eq!(frame.family, QueueFamily::Governance);
        assert_eq!(frame.bytes, 7);
    }

    #[tokio::test]
    async fn dropped_receiver_marks_queue_closed() {
        // Mirror mpsc's contract: dropping the consumer makes future pushes
        // observe `Closed`, so a peer reader can break out cleanly.
        let (tx, rx) = BytePriorityQueue::channel(1024, 1024);
        drop(rx);
        let p = peer(0x0A);
        assert_eq!(
            tx.try_push(p, vec![0u8; 1], QueueFamily::Governance),
            Err(PushError::Closed)
        );
    }

    // --- D3 mapping pinned: every SyncMessage variant ------------------------

    #[test]
    fn classify_sync_message_matches_the_v1_mapping_table() {
        let room = RoomId::from_bytes([0x11; 32]);
        let id = EventId::from_bytes([0xaa; 32]);

        // Governance family / Governance priority / peer-only bucket.
        assert_eq!(
            classify_sync_message(&SyncMessage::AdminTip {
                room_id: room,
                tip: None
            }),
            QueueFamily::Governance
        );
        assert_eq!(
            classify_sync_message(&SyncMessage::WantMembership {
                room_id: room,
                have: vec![]
            }),
            QueueFamily::Governance
        );
        assert_eq!(
            QueueFamily::Governance.priority(),
            QueuePriority::Governance
        );
        assert!(!QueueFamily::Governance.charges_stream_budget());

        // Checkpoint family (Heads) and Session family (ProveCapability) both
        // map to Checkpoint priority and the peer-only bucket.
        assert_eq!(
            classify_sync_message(&SyncMessage::Heads {
                room_id: room,
                heads: vec![id]
            }),
            QueueFamily::Checkpoint
        );
        assert_eq!(
            classify_sync_message(&SyncMessage::ProveCapability {
                room_id: room,
                invite_id: [0x3c; 16],
                capability_secret: [0x5e; 16]
            }),
            QueueFamily::Session
        );
        assert_eq!(
            QueueFamily::Checkpoint.priority(),
            QueuePriority::Checkpoint
        );
        assert_eq!(QueueFamily::Session.priority(), QueuePriority::Checkpoint);
        assert!(!QueueFamily::Session.charges_stream_budget());

        // Content priority / stream-charging families.
        assert_eq!(
            classify_sync_message(&SyncMessage::Events {
                room_id: room,
                frames: vec![]
            }),
            QueueFamily::Publication
        );
        assert_eq!(
            classify_sync_message(&SyncMessage::WantRecentChat {
                room_id: room,
                window: Window {
                    max_count: 10,
                    since_ms: None
                },
                have: vec![]
            }),
            QueueFamily::Subscription
        );
        assert_eq!(QueueFamily::Publication.priority(), QueuePriority::Content);
        assert_eq!(QueueFamily::Subscription.priority(), QueuePriority::Content);
        assert!(QueueFamily::Publication.charges_stream_budget());
        assert!(QueueFamily::Subscription.charges_stream_budget());

        // BlobHints priority / stream-charging families.
        assert_eq!(
            classify_sync_message(&SyncMessage::WantEvents {
                room_id: room,
                ids: vec![id]
            }),
            QueueFamily::Reconciliation
        );
        assert_eq!(
            classify_sync_message(&SyncMessage::NotFound {
                room_id: room,
                ids: vec![id]
            }),
            QueueFamily::Reconciliation
        );
        assert_eq!(
            QueueFamily::Reconciliation.priority(),
            QueuePriority::BlobHints
        );
        assert!(QueueFamily::Reconciliation.charges_stream_budget());
    }
}
