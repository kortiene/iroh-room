//! The composed pipe **connect gate** (`PHASE-0-SPIKE.md` Membership §5; spec §4.3).
//!
//! [`evaluate`] is the single place the stage-2 / teardown decision is composed: it
//! layers the already-landed pure predicate
//! [`pipe_connect_allowed`](iroh_rooms_core::membership::pipe_connect_allowed)
//! (identity → Active → `allowed_members` → owner-Active → expiry) on top of the
//! Pipe-plane-owned checks the predicate explicitly defers to its caller (§5):
//! the locally-registered loopback **target**, the governing `pipe.opened`, and the
//! `pipe.closed`-causally-known status. Every lookup **fails closed** (a missing
//! target / unknown pipe / unreachable engine denies as `Closed`), and the decision
//! is always made against the **current** snapshot — never an ancestor view (D6).
//!
//! The same function backs both the accept handler (one new stream) and the
//! teardown watcher (each live session, each tick), so a session is judged by
//! exactly the rule a fresh connect would face.

use std::net::SocketAddr;

use iroh_rooms_core::event::keys::DeviceKey;
use iroh_rooms_core::membership::{pipe_connect_allowed, PipeDecision};

use super::audit::PipeDenyCause;
use super::registry::PipeRegistry;
use super::runtime::PipeQuery;

/// The gate decision for a pipe stream / live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeGateVerdict {
    /// Accept: forward to this loopback target.
    Accept(SocketAddr),
    /// Deny, with the specific audit cause.
    Reject(PipeDenyCause),
}

/// Evaluate the full pipe connect gate for `device` against `pipe_id`, composing
/// the local registry/closed/opened checks with the pure
/// [`pipe_connect_allowed`] predicate. `now_ms` is consulted only to deny on expiry
/// (the one wall-clock use, §5).
pub async fn evaluate(
    query: &PipeQuery,
    registry: &PipeRegistry,
    device: &DeviceKey,
    pipe_id: [u8; 16],
    now_ms: u64,
) -> PipeGateVerdict {
    // The real forward target lives only in the owner's local registry. Its absence
    // means "not an open pipe here" (or already closed) — fail closed.
    let Some(target) = registry.target(&pipe_id) else {
        return PipeGateVerdict::Reject(PipeDenyCause::Closed);
    };
    // A causally-known `pipe.closed` voids the pipe (§5).
    if query.pipe_is_closed(pipe_id).await {
        return PipeGateVerdict::Reject(PipeDenyCause::Closed);
    }
    // The governing announcement supplies `allowed_members` / `owner_id` / expiry.
    let Some(opened) = query.pipe_opened(pipe_id).await else {
        return PipeGateVerdict::Reject(PipeDenyCause::Closed);
    };
    // The current snapshot drives identity → Active → allowed → owner-Active.
    let Some(snapshot) = query.snapshot().await else {
        return PipeGateVerdict::Reject(PipeDenyCause::Closed);
    };
    match pipe_connect_allowed(&snapshot, device, &opened, Some(now_ms)) {
        PipeDecision::Accept => PipeGateVerdict::Accept(target),
        PipeDecision::Reject(reason) => PipeGateVerdict::Reject(reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use iroh_rooms_core::event::content::PipeOpened;
    use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey};
    use tokio::sync::mpsc;

    use super::{evaluate, PipeGateVerdict};
    use crate::pipe::audit::PipeDenyCause;
    use crate::pipe::registry::PipeRegistry;
    use crate::pipe::runtime::{PipeQuery, PipeQueryMsg};

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    fn device_key(seed: u8) -> DeviceKey {
        DeviceKey::from_bytes([seed; 32])
    }

    fn identity_key(seed: u8) -> IdentityKey {
        IdentityKey::from_bytes([seed; 32])
    }

    fn minimal_opened(pipe_id: [u8; 16]) -> PipeOpened {
        PipeOpened {
            pipe_id,
            owner_id: identity_key(0x01),
            owner_endpoint: device_key(0x81),
            kind: "tcp".to_owned(),
            label: "gate-test".to_owned(),
            target_hint: "localhost:9000".to_owned(),
            alpn: "/iroh-rooms/pipe/1".to_owned(),
            allowed_members: vec![identity_key(0x10)],
            expires_at: None,
        }
    }

    /// A `PipeQuery` backed by a dropped receiver — every send immediately errors.
    fn dead_query() -> PipeQuery {
        let (tx, rx) = mpsc::channel::<PipeQueryMsg>(8);
        drop(rx);
        PipeQuery::new(tx)
    }

    const PIPE_ID: [u8; 16] = [0x42; 16];

    // ── fail-closed: no target in registry → Reject(Closed) before any I/O ────

    #[tokio::test]
    async fn rejects_closed_when_registry_has_no_target() {
        let reg = PipeRegistry::new(); // empty
        let q = dead_query();
        let dev = device_key(0x10);
        assert_eq!(
            evaluate(&q, &reg, &dev, PIPE_ID, 0).await,
            PipeGateVerdict::Reject(PipeDenyCause::Closed),
            "no target in registry must deny as Closed before querying the engine"
        );
    }

    // ── fail-closed: pipe_is_closed → true → Reject(Closed) ────────────────────

    #[tokio::test]
    async fn rejects_closed_when_engine_reports_pipe_closed() {
        let reg = PipeRegistry::new();
        reg.insert(minimal_opened(PIPE_ID), loopback(19001))
            .expect("insert");
        let (tx, mut rx) = mpsc::channel::<PipeQueryMsg>(8);
        let q = PipeQuery::new(tx);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let PipeQueryMsg::IsClosed(_, reply) = msg {
                    let _ = reply.send(true); // engine: this pipe is closed
                }
            }
        });
        let dev = device_key(0x10);
        assert_eq!(
            evaluate(&q, &reg, &dev, PIPE_ID, 0).await,
            PipeGateVerdict::Reject(PipeDenyCause::Closed),
        );
    }

    // ── fail-closed: pipe_opened returns None → Reject(Closed) ─────────────────

    #[tokio::test]
    async fn rejects_closed_when_pipe_opened_is_absent() {
        let reg = PipeRegistry::new();
        reg.insert(minimal_opened(PIPE_ID), loopback(19002))
            .expect("insert");
        let (tx, mut rx) = mpsc::channel::<PipeQueryMsg>(8);
        let q = PipeQuery::new(tx);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    PipeQueryMsg::IsClosed(_, reply) => {
                        let _ = reply.send(false);
                    }
                    PipeQueryMsg::Opened(_, reply) => {
                        let _ = reply.send(None); // engine: no pipe.opened known yet
                    }
                    PipeQueryMsg::Snapshot(_) => {} // unreached: denied at Opened==None
                }
            }
        });
        let dev = device_key(0x10);
        assert_eq!(
            evaluate(&q, &reg, &dev, PIPE_ID, 0).await,
            PipeGateVerdict::Reject(PipeDenyCause::Closed),
        );
    }

    // ── fail-closed: snapshot returns None → Reject(Closed) ────────────────────

    #[tokio::test]
    async fn rejects_closed_when_snapshot_is_absent() {
        let reg = PipeRegistry::new();
        reg.insert(minimal_opened(PIPE_ID), loopback(19003))
            .expect("insert");
        let (tx, mut rx) = mpsc::channel::<PipeQueryMsg>(8);
        let q = PipeQuery::new(tx);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    PipeQueryMsg::IsClosed(_, reply) => {
                        let _ = reply.send(false);
                    }
                    PipeQueryMsg::Opened(_, reply) => {
                        let _ = reply.send(Some(minimal_opened(PIPE_ID)));
                    }
                    PipeQueryMsg::Snapshot(reply) => {
                        drop(reply); // engine unable to answer — snapshot is gone
                    }
                }
            }
        });
        let dev = device_key(0x10);
        assert_eq!(
            evaluate(&q, &reg, &dev, PIPE_ID, 0).await,
            PipeGateVerdict::Reject(PipeDenyCause::Closed),
        );
    }
}
