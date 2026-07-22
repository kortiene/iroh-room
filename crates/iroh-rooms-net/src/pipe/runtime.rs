//! [`PipeQuery`] — the Pipe plane's read bridge to the single-owner engine
//! (spec §6.5 / D5).
//!
//! The pipe accept-handler and the teardown watcher both need three reads against
//! the engine's **current** state — the membership snapshot, the governing
//! `pipe.opened`, and whether a `pipe.closed` is causally known. The engine is
//! single-owner (the [`Node`](crate::node::Node) pump task), so these reads are
//! marshalled over a channel and answered by the pump, exactly like the `Node`'s
//! own query commands. The handler/watcher hold a cheap, cloneable [`PipeQuery`];
//! the pump holds the receiver.
//!
//! Every read **fails closed**: if the pump is gone (shutdown), the bounded
//! channel is saturated, or a reply is dropped, `snapshot`/`pipe_opened` return
//! `None` and `pipe_is_closed` returns `true`, so a racing teardown/accept denies
//! rather than leaks.
//!
//! Issue #141: the channel is **bounded** (`NetConfig::pipe_query_capacity`,
//! default `MAX_CONCURRENT_BIDI_STREAMS`). A pipe handler services a
//! network-derived ALPN, so its control queries cannot sit on an unbounded
//! queue a remote peer could pressure. Saturation fails closed — the same
//! outcome as a vanished pump — so authorization decisions never branch on
//! queue state.

use iroh_rooms_core::event::content::PipeOpened;
use iroh_rooms_core::membership::MembershipSnapshot;
use tokio::sync::{mpsc, oneshot};

/// A read request from the Pipe plane to the engine pump.
pub enum PipeQueryMsg {
    /// The current convergent membership snapshot.
    Snapshot(oneshot::Sender<MembershipSnapshot>),
    /// The governing `pipe.opened` for a `pipe_id`, if known.
    Opened([u8; 16], oneshot::Sender<Option<PipeOpened>>),
    /// Whether a `pipe.closed` for a `pipe_id` is causally known.
    IsClosed([u8; 16], oneshot::Sender<bool>),
}

/// The Pipe plane's cloneable read handle onto the engine.
#[derive(Clone)]
pub struct PipeQuery {
    tx: mpsc::Sender<PipeQueryMsg>,
}

impl std::fmt::Debug for PipeQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PipeQuery")
    }
}

impl PipeQuery {
    /// Build a query handle over the bounded channel the pump drains.
    ///
    /// Exposed for unit tests that need a handle backed by a controlled channel.
    #[must_use]
    pub fn new(tx: mpsc::Sender<PipeQueryMsg>) -> Self {
        Self { tx }
    }

    /// The current membership snapshot, or `None` if the engine is gone or the
    /// query channel is saturated (fail-closed: the caller denies on `None`).
    pub async fn snapshot(&self) -> Option<MembershipSnapshot> {
        let (reply, rx) = oneshot::channel();
        if self.tx.try_send(PipeQueryMsg::Snapshot(reply)).is_err() {
            return None;
        }
        rx.await.ok()
    }

    /// The governing `pipe.opened` for `pipe_id`, or `None` if unknown / the
    /// engine is gone / the query channel is saturated.
    pub async fn pipe_opened(&self, pipe_id: [u8; 16]) -> Option<PipeOpened> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .try_send(PipeQueryMsg::Opened(pipe_id, reply))
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Whether a `pipe.closed` for `pipe_id` is causally known. **Fail-closed**:
    /// returns `true` (treat as closed) if the engine is gone, the query channel
    /// is saturated, or a read fails.
    pub async fn pipe_is_closed(&self, pipe_id: [u8; 16]) -> bool {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .try_send(PipeQueryMsg::IsClosed(pipe_id, reply))
            .is_err()
        {
            return true;
        }
        rx.await.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::{PipeQuery, PipeQueryMsg};

    /// Test-channel capacity: large enough that the test fixtures (which each
    /// send a single query) never saturate it, while still exercising the
    /// bounded-`Sender` shape introduced in #141.
    const TEST_CHANNEL_CAPACITY: usize = 8;

    fn saturated_query() -> (PipeQuery, mpsc::Receiver<PipeQueryMsg>) {
        let (tx, rx) = mpsc::channel::<PipeQueryMsg>(1);
        let (reply, _reply_rx) = tokio::sync::oneshot::channel();
        tx.try_send(PipeQueryMsg::Snapshot(reply))
            .expect("fixture fills the query channel");
        (PipeQuery::new(tx), rx)
    }

    // ── fail-closed when the pump (mpsc receiver) is gone ──────────────────────

    #[tokio::test]
    async fn snapshot_is_none_when_pump_is_gone() {
        let (tx, rx) = mpsc::channel::<PipeQueryMsg>(TEST_CHANNEL_CAPACITY);
        drop(rx); // engine gone
        let q = PipeQuery::new(tx);
        assert!(
            q.snapshot().await.is_none(),
            "fail-closed: None when pump gone"
        );
    }

    #[tokio::test]
    async fn pipe_opened_is_none_when_pump_is_gone() {
        let (tx, rx) = mpsc::channel::<PipeQueryMsg>(TEST_CHANNEL_CAPACITY);
        drop(rx);
        let q = PipeQuery::new(tx);
        assert!(
            q.pipe_opened([0x01; 16]).await.is_none(),
            "fail-closed: None when pump gone"
        );
    }

    #[tokio::test]
    async fn is_closed_is_true_when_pump_is_gone() {
        let (tx, rx) = mpsc::channel::<PipeQueryMsg>(TEST_CHANNEL_CAPACITY);
        drop(rx);
        let q = PipeQuery::new(tx);
        assert!(
            q.pipe_is_closed([0x01; 16]).await,
            "fail-closed: treat-as-closed when pump gone"
        );
    }

    // ── fail-closed immediately when the bounded channel is full ─────────────

    #[tokio::test]
    async fn snapshot_is_none_when_query_channel_is_full() {
        let (q, _rx) = saturated_query();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), q.snapshot())
            .await
            .expect("snapshot must not wait for query-channel capacity");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn pipe_opened_is_none_when_query_channel_is_full() {
        let (q, _rx) = saturated_query();
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), q.pipe_opened([0x01; 16]))
                .await
                .expect("pipe_opened must not wait for query-channel capacity");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn is_closed_is_true_when_query_channel_is_full() {
        let (q, _rx) = saturated_query();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            q.pipe_is_closed([0x01; 16]),
        )
        .await
        .expect("pipe_is_closed must not wait for query-channel capacity");
        assert!(result);
    }

    // ── fail-closed when the pump receives but drops the oneshot sender ─────────

    #[tokio::test]
    async fn snapshot_is_none_when_pump_drops_reply_sender() {
        let (tx, mut rx) = mpsc::channel::<PipeQueryMsg>(TEST_CHANNEL_CAPACITY);
        let q = PipeQuery::new(tx);
        tokio::spawn(async move {
            if let Some(PipeQueryMsg::Snapshot(reply)) = rx.recv().await {
                drop(reply); // pump crashes before replying
            }
        });
        assert!(q.snapshot().await.is_none());
    }

    #[tokio::test]
    async fn is_closed_is_true_when_pump_drops_reply_sender() {
        let (tx, mut rx) = mpsc::channel::<PipeQueryMsg>(TEST_CHANNEL_CAPACITY);
        let q = PipeQuery::new(tx);
        tokio::spawn(async move {
            if let Some(PipeQueryMsg::IsClosed(_, reply)) = rx.recv().await {
                drop(reply);
            }
        });
        assert!(q.pipe_is_closed([0x01; 16]).await);
    }

    #[tokio::test]
    async fn pipe_opened_is_none_when_pump_drops_reply_sender() {
        let (tx, mut rx) = mpsc::channel::<PipeQueryMsg>(TEST_CHANNEL_CAPACITY);
        let q = PipeQuery::new(tx);
        tokio::spawn(async move {
            if let Some(PipeQueryMsg::Opened(_, reply)) = rx.recv().await {
                drop(reply);
            }
        });
        assert!(q.pipe_opened([0x01; 16]).await.is_none());
    }
}
