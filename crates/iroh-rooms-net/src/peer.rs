//! Per-peer I/O: the reader/writer tasks bound to one bidi stream, and the
//! dial-with-backoff loop that keeps an outbound link to a member alive
//! (`PHASE-0-SPIKE.md` ADR-1 per-room peer manager; spec §4.7 / G2 / G5).
//!
//! A single QUIC bidi stream is full-duplex, so one connection per pair carries
//! traffic **both** ways: the dialer's `open_bi()` and the acceptor's
//! `accept_bi()` yield the two ends of the same logical stream. Each side runs a
//! [`reader_task`] (frames → engine inbound sink) and a [`writer_task`] (engine
//! outbound queue → frames). The [`dial_loop`] owns the outbound side's lifecycle
//! and reconnect; the accept handler owns the inbound side's.

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{ApplicationClose, Connection, ConnectionError, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::sync::PeerId;
use tokio::sync::mpsc;

use crate::admission::AdmissionDecision;
use crate::alpn::EVENT_ALPN;
use crate::frame::{read_frame, write_frame};
use crate::handler::REJECT_CODE;
use crate::state::PeerConnState;
use crate::transport::{Inbound, Shared};

/// Bridge an iroh [`EndpointId`] (device key) to the engine's [`PeerId`] — both
/// are the same raw 32 `device_id` bytes (Membership §1).
#[must_use]
pub(crate) fn peer_id(device: EndpointId) -> PeerId {
    PeerId::from_bytes(*device.as_bytes())
}

/// Wire up a live connection: register the outbound queue + connection handle and
/// spawn the reader and writer tasks. Called by both the accept handler (inbound)
/// and [`dial_loop`] (outbound). The outbound queue is registered **before** the
/// caller flips the peer to `Connected`, so the engine's `on_connect` handshake
/// always finds a writer.
pub(crate) fn register_connection(
    shared: Arc<Shared>,
    device: EndpointId,
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
) {
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    shared.register_outbound(device, tx);
    shared.register_connection(device, conn);

    tokio::spawn(writer_task(send, rx));
    tokio::spawn(reader_task(shared, device, recv));
}

/// Drain the per-peer outbound frame queue, writing each body as a length-prefixed
/// frame. Ends when the queue's sender is dropped (peer unregistered) or a write
/// fails (link down).
async fn writer_task(mut send: SendStream, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(body) = rx.recv().await {
        if let Err(err) = write_frame(&mut send, &body).await {
            tracing::warn!(%err, "writer: frame write failed; closing stream");
            break;
        }
    }
    // Best-effort clean finish; the peer may already be gone.
    let _ = send.finish();
}

/// Read length-prefixed frames and hand each **verbatim** to the engine inbound
/// sink (the engine validates; the transport never decodes). Ends on clean EOF or
/// a frame error.
async fn reader_task(shared: Arc<Shared>, device: EndpointId, mut recv: RecvStream) {
    let peer = peer_id(device);
    loop {
        match read_frame(&mut recv).await {
            Ok(Some(bytes)) => {
                if shared.inbound_tx.send(Inbound { peer, bytes }).is_err() {
                    // The driver/engine is gone; nothing left to feed.
                    break;
                }
            }
            Ok(None) => break, // clean EOF at a frame boundary
            Err(err) => {
                tracing::debug!(%err, peer = %device, "reader: frame error; ending");
                break;
            }
        }
    }
}

/// Dial `addr` and keep the link alive: authorize the proven remote id, open the
/// bidi stream, register the peer `Connected`, then block until the connection
/// closes and redial with bounded exponential backoff + jitter. Never gives up
/// while the task lives (the caller aborts it on `Drop` / roster change, spec N6).
///
/// A dial that reaches a **non-member** (the proven `remote_id()` fails admission)
/// records `Unauthorized` and stops — there is no point redialing a peer we will
/// never admit.
pub(crate) async fn dial_loop(shared: Arc<Shared>, endpoint: Endpoint, addr: EndpointAddr) {
    let target = addr.id;
    let mut attempt: u32 = 0;

    loop {
        shared.table.set(target, PeerConnState::Connecting, None);

        match endpoint.connect(addr.clone(), EVENT_ALPN).await {
            Ok(conn) => {
                // Authorize the cryptographically-proven remote id, not the dialed
                // hint: a dial can land on a non-member (or a rebound key).
                let remote = conn.remote_id();
                match shared.admission.authorize(remote) {
                    AdmissionDecision::Reject(cause) => {
                        shared.audit.rejected(remote, cause);
                        shared.table.set(remote, PeerConnState::Unauthorized, None);
                        conn.close(REJECT_CODE, b"unauthorized-remote");
                        return; // not a member: stop dialing
                    }
                    AdmissionDecision::AdmitProvisional => {
                        // The dialer never bootstraps a join: a joiner dials an
                        // *Active* admin (its gate admits the admin normally), so a
                        // provisional verdict here only arises if a join-hosting node
                        // dials an unknown device. Treat it as not-yet-reachable —
                        // do not establish a link — and back off rather than opening
                        // a stream to a peer we cannot place as a member.
                        tracing::debug!(
                            peer = %remote,
                            "dial: remote only provisionally admissible; backing off without establishing"
                        );
                        shared.table.set(remote, PeerConnState::Offline, None);
                    }
                    AdmissionDecision::Admit { identity } => {
                        match establish_outbound(&shared, &conn, remote, identity).await {
                            Established::Up => {
                                attempt = 0;
                                // Block until the link drops.
                                conn.closed().await;
                                shared.unregister(remote);
                                if rejected_by_remote(&conn) {
                                    // The remote refused us mid-stream: a roster
                                    // change, not a transient drop. Stop redialing.
                                    shared.table.set(remote, PeerConnState::Unauthorized, None);
                                    return;
                                }
                                shared.table.set(remote, PeerConnState::Offline, None);
                                shared.audit.disconnected(remote);
                            }
                            Established::RemoteRejected => {
                                // The remote's accept-gate refused us (stable REJECT
                                // close). Surface Unauthorized and stop dialing —
                                // reachability is not the problem (spec §4.6).
                                shared
                                    .audit
                                    .rejected(remote, crate::admission::RejectCause::NotActive);
                                shared.table.set(remote, PeerConnState::Unauthorized, None);
                                return;
                            }
                            Established::Failed => {
                                shared.table.set(remote, PeerConnState::Offline, None);
                            }
                        }
                    }
                }
            }
            Err(err) => {
                tracing::debug!(%err, peer = %target, "dial: connect failed");
                shared.table.set(target, PeerConnState::Offline, None);
            }
        }

        attempt = attempt.saturating_add(1);
        tokio::time::sleep(backoff(attempt, &shared.me)).await;
    }
}

/// Outcome of trying to bring up an outbound link after a successful dial.
enum Established {
    /// The bidi stream is up; the caller waits on `conn.closed()`.
    Up,
    /// The remote's accept-gate refused us (stable REJECT close): stop dialing.
    RemoteRejected,
    /// A transient failure (stream open error, not a reject): redial.
    Failed,
}

/// Open the bidi stream for an admitted outbound dial and register the peer.
async fn establish_outbound(
    shared: &Arc<Shared>,
    conn: &Connection,
    remote: EndpointId,
    identity: IdentityKey,
) -> Established {
    match conn.open_bi().await {
        Ok((send, recv)) => {
            shared.audit.accepted(remote, &identity);
            register_connection(shared.clone(), remote, conn.clone(), send, recv);
            shared
                .table
                .set(remote, PeerConnState::Connected, Some(identity));
            shared.audit.connected(remote);
            Established::Up
        }
        Err(err) => {
            if rejected_by_remote(conn) {
                tracing::warn!(peer = %remote, "dial: remote refused admission (reject close)");
                Established::RemoteRejected
            } else {
                tracing::warn!(%err, peer = %remote, "dial: open_bi failed");
                shared.table.set(remote, PeerConnState::Offline, None);
                Established::Failed
            }
        }
    }
}

/// Did the remote close this connection with the stable
/// [`REJECT_CODE`](crate::handler::REJECT_CODE) admission-reject code? If so, the
/// dialer surfaces `Unauthorized` rather than a generic drop (spec §4.6).
fn rejected_by_remote(conn: &Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. }))
            if error_code == REJECT_CODE
    )
}

/// Bounded exponential backoff with deterministic, dependency-free jitter
/// (250 ms → 8 s, plus up to a quarter-interval of jitter). `attempt` is 1 on the
/// first redial after a drop, so reconnects start fast.
fn backoff(attempt: u32, me: &EndpointId) -> Duration {
    const BASE_MS: u64 = 250;
    const CAP_MS: u64 = 8_000;

    let shift = attempt.saturating_sub(1).min(5); // 250,500,1000,2000,4000,8000
    let capped = (BASE_MS << shift).min(CAP_MS);
    // Cheap jitter from our id byte + attempt — avoids an rng dependency while
    // still de-synchronizing simultaneous reconnects (spec §4.7).
    let seed = u64::from(me.as_bytes()[0]) ^ u64::from(attempt).wrapping_mul(2_654_435_761);
    let jitter = seed % (capped / 4 + 1);
    Duration::from_millis(capped.saturating_add(jitter))
}

#[cfg(test)]
mod tests {
    use super::{backoff, peer_id};
    use iroh::{EndpointId, SecretKey};
    use std::time::Duration;

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    // --- peer_id: Membership §1 identity-unification invariant ---

    #[test]
    fn peer_id_bytes_equal_endpoint_id_bytes() {
        // device_id == EndpointId byte-for-byte (Membership §1 / spec A2). The
        // engine uses PeerId and the transport uses EndpointId; they must agree.
        let dev = device(7);
        let pid = peer_id(dev);
        assert_eq!(
            pid.as_bytes(),
            dev.as_bytes(),
            "peer_id must preserve the raw 32 device-id bytes"
        );
    }

    #[test]
    fn peer_id_distinct_devices_produce_distinct_ids() {
        assert_ne!(peer_id(device(1)), peer_id(device(2)));
    }

    // --- backoff: bounds and capping ---

    #[test]
    fn backoff_attempt_1_is_in_range_250_to_312_ms() {
        // attempt=1: shift=0, capped=250ms, jitter ≤ 250/4 = 62ms → [250, 312]
        let dur = backoff(1, &device(1));
        assert!(
            dur >= Duration::from_millis(250),
            "attempt 1 must be ≥ 250 ms, got {dur:?}"
        );
        assert!(
            dur <= Duration::from_millis(312),
            "attempt 1 must be ≤ 312 ms, got {dur:?}"
        );
    }

    #[test]
    fn backoff_caps_at_8s_base_after_attempt_6() {
        // attempt≥6: shift=5, capped=8000ms, jitter ≤ 8000/4 = 2000ms → [8000, 10000]
        for attempt in [6u32, 7, 10, 100] {
            let dur = backoff(attempt, &device(2));
            assert!(
                dur >= Duration::from_secs(8),
                "attempt {attempt} must be ≥ 8 s, got {dur:?}"
            );
            assert!(
                dur <= Duration::from_secs(10),
                "attempt {attempt} must be ≤ 10 s (8 s + jitter cap), got {dur:?}"
            );
        }
    }

    #[test]
    fn backoff_attempt_6_exceeds_attempt_1_for_all_endpoints() {
        // d1 max = 312ms, d6 min = 8000ms → d6 > d1 always, regardless of seed.
        for seed in [0x00u8, 0x01, 0x80, 0xFF] {
            let dev = device(seed);
            let d1 = backoff(1, &dev);
            let d6 = backoff(6, &dev);
            assert!(
                d6 > d1,
                "attempt 6 must always exceed attempt 1 (seed {seed:#04x}): d1={d1:?}, d6={d6:?}"
            );
        }
    }

    #[test]
    fn backoff_jitter_differs_across_distinct_endpoint_ids() {
        // Different endpoint ids de-synchronize simultaneous reconnects (spec §4.7).
        // Endpoints [0x01; 32] and [0x02; 32] have different first bytes, so the
        // seed (me_byte_0 XOR knuth_hash(attempt)) differs → different jitter.
        let dur_a = backoff(1, &device(0x01));
        let dur_b = backoff(1, &device(0x02));
        assert_ne!(
            dur_a, dur_b,
            "different endpoint ids must produce different backoff jitter"
        );
    }

    // --- Edge cases ---

    #[test]
    fn backoff_attempt_zero_is_at_least_250ms() {
        // attempt=0: saturating_sub(1)=0, shift=0, base=250 ms.
        let dur = backoff(0, &device(1));
        assert!(
            dur >= Duration::from_millis(250),
            "attempt 0 must yield at least the 250 ms base, got {dur:?}"
        );
    }

    #[test]
    fn backoff_base_doubles_from_attempt_1_to_5() {
        // Attempts 1..5 double the base each step (250→500→1000→2000→4000 ms).
        // Use endpoint 0x55 which has a deterministic jitter smaller than the
        // inter-step gap, so strict ordering holds.
        let dev = device(0x55);
        let d: Vec<Duration> = (1u32..=5).map(|a| backoff(a, &dev)).collect();
        for w in d.windows(2) {
            assert!(
                w[0] < w[1],
                "backoff must strictly increase: attempt n={:?} vs n+1={:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn backoff_at_u32_max_does_not_panic() {
        // Saturating arithmetic keeps every intermediate result defined.
        let _ = backoff(u32::MAX, &device(0xFF));
    }

    #[test]
    fn backoff_attempt_zero_is_same_range_as_attempt_one() {
        // attempt=0 uses shift=0 (same as attempt=1) so both fall in [250, 312].
        let dur = backoff(0, &device(0x42));
        assert!(dur >= Duration::from_millis(250));
        assert!(dur <= Duration::from_millis(312));
    }
}
