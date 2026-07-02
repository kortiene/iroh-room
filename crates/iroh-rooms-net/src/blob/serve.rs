//! The blob-plane serve gate (IR-0204 spec §5.3): the two-gate ACL over
//! `iroh-blobs` `provider::events`, lifted from
//! `spike-blobs::net::spawn_event_gate` and re-pointed at a live
//! [`BlobAclView`](super::BlobAclView) cell instead of a fixed fixture.
//!
//! - **Gate 1 — per-node admission** (`ClientConnected`): accept iff the
//!   QUIC/TLS-proven `endpoint_id` is a current active member; else
//!   `AbortReason::Permission`. Fail-closed on an unauthenticated peer.
//! - **Gate 2 — per-hash authorization** (`GetRequestReceived` /
//!   `GetManyRequestReceived`): serve a hash only if it is referenced by a valid
//!   `file.shared` in the room.
//! - **Push / observe** are always denied — a peer can never write to or
//!   enumerate the provider's store over the blobs ALPN. `ObserveMode` has no
//!   `Disabled` variant (unlike `RequestMode`), so this gate explicitly
//!   intercepts and rejects every observe request rather than relying on a
//!   default mode whose "no interception" semantics are ambiguous for that event
//!   kind.
//!
//! `RequestReceived` messages carry only a `connection_id` (no `endpoint_id`), so
//! the gate tracks `connection_id -> EndpointId` from the `ClientConnected` /
//! `ConnectionClosed` pair to attribute the `blob.serve.*` audit lines to a peer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use iroh::EndpointId;
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage, RequestMode,
};
use tokio::task::JoinHandle;

use crate::audit::{AuditSink, BlobDenyCause};

use super::BlobAclView;

/// Event-channel capacity for the provider gate loop (mirrors the spike; MVP room
/// sizes make backpressure here a non-issue).
const GATE_CHANNEL_CAPACITY: usize = 64;

/// Drive the `iroh-blobs` provider-events channel as the two-gate ACL (spec §5.3).
///
/// Returns the [`EventSender`] to hand to [`super::BlobStore::serve_handler`] and
/// the task running the decision loop. The task ends when the provider (and thus
/// the event channel) is dropped.
#[must_use]
pub fn spawn_blob_gate(
    acl: Arc<Mutex<BlobAclView>>,
    audit: Arc<dyn AuditSink>,
) -> (EventSender, JoinHandle<()>) {
    // Gate 1 via `connected = Intercept`; Gate 2 via `get`/`get_many = Intercept`.
    // `push` inherits `EventMask::DEFAULT`'s `Disabled` (no writes, ever); `observe`
    // is explicitly `Intercept` + always-deny below (see module doc).
    let mask = EventMask {
        connected: ConnectMode::Intercept,
        get: RequestMode::Intercept,
        get_many: RequestMode::Intercept,
        observe: ObserveMode::Intercept,
        ..EventMask::DEFAULT
    };
    let (events, mut rx) = EventSender::channel(GATE_CHANNEL_CAPACITY, mask);

    let handle = tokio::spawn(async move {
        // `connection_id -> endpoint_id`, populated on Gate 1 accept and cleared on
        // disconnect, so a per-hash decision (Gate 2) can still be attributed to a
        // peer for the audit sink even though `RequestReceived` carries no identity.
        let mut peers: HashMap<u64, EndpointId> = HashMap::new();

        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::ClientConnected(msg) => {
                    let active = msg.endpoint_id.is_some_and(|id| {
                        acl.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .is_active(id)
                    });
                    let decision = match msg.endpoint_id {
                        Some(id) if active => {
                            peers.insert(msg.connection_id, id);
                            Ok(())
                        }
                        Some(id) => {
                            audit.blob_serve_rejected(id, BlobDenyCause::NotActive, None);
                            Err(AbortReason::Permission)
                        }
                        // Fail-closed: an unauthenticated peer has no identity to
                        // audit against.
                        None => Err(AbortReason::Permission),
                    };
                    msg.tx.send(decision).await.ok();
                }
                ProviderMessage::ConnectionClosed(msg) => {
                    peers.remove(&msg.connection_id);
                }
                ProviderMessage::GetRequestReceived(msg) => {
                    let hash = *msg.request.hash.as_bytes();
                    let referenced = acl
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_referenced(&hash);
                    let peer = peers.get(&msg.connection_id).copied();
                    let decision = if referenced {
                        if let Some(peer) = peer {
                            audit.blob_serve_accepted(peer, hash);
                        }
                        Ok(())
                    } else {
                        if let Some(peer) = peer {
                            audit.blob_serve_rejected(
                                peer,
                                BlobDenyCause::NotReferenced,
                                Some(hash),
                            );
                        }
                        Err(AbortReason::Permission)
                    };
                    msg.tx.send(decision).await.ok();
                }
                ProviderMessage::GetManyRequestReceived(msg) => {
                    let all_referenced = {
                        let view = acl
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        msg.request
                            .hashes
                            .iter()
                            .all(|h| view.is_referenced(h.as_bytes()))
                    };
                    let peer = peers.get(&msg.connection_id).copied();
                    let decision = if all_referenced {
                        Ok(())
                    } else {
                        if let Some(peer) = peer {
                            audit.blob_serve_rejected(peer, BlobDenyCause::NotReferenced, None);
                        }
                        Err(AbortReason::Permission)
                    };
                    msg.tx.send(decision).await.ok();
                }
                // The room never permits writing to or enumerating the store over
                // the blobs ALPN.
                ProviderMessage::PushRequestReceived(msg) => {
                    if let Some(peer) = peers.get(&msg.connection_id).copied() {
                        audit.blob_serve_rejected(peer, BlobDenyCause::PushDenied, None);
                    }
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::ObserveRequestReceived(msg) => {
                    if let Some(peer) = peers.get(&msg.connection_id).copied() {
                        audit.blob_serve_rejected(peer, BlobDenyCause::ObserveDenied, None);
                    }
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::Throttle(msg) => {
                    msg.tx.send(Ok(())).await.ok();
                }
                _ => {}
            }
        }
    });

    (events, handle)
}

#[cfg(test)]
mod tests {
    use super::spawn_blob_gate;
    use crate::audit::TracingAudit;
    use crate::blob::BlobAclView;
    use std::sync::{Arc, Mutex};

    // A smoke test that the gate spawns and shuts down cleanly when its channel is
    // dropped; the two-gate decision logic itself is proven by the always-green
    // Node-level `blob_e2e` integration suite (real `ClientConnected`/
    // `GetRequestReceived` messages require a live `iroh-blobs` connection, which a
    // unit test cannot synthesize without the provider/getter machinery).
    #[tokio::test]
    async fn gate_task_ends_when_event_sender_is_dropped() {
        let acl = Arc::new(Mutex::new(BlobAclView::empty()));
        let (events, handle) = spawn_blob_gate(acl, Arc::new(TracingAudit));
        drop(events);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("gate task must end once the EventSender is dropped")
            .expect("gate task must not panic");
    }
}
