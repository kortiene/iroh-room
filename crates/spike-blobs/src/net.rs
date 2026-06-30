//! The live wiring: an `iroh-blobs` provider gated by [`AuthContext`], and a
//! fetcher that consumes a `file.shared`, fetches, and verifies.
//!
//! The two enforcement points (`PHASE-0-SPIKE.md` §5) are implemented in
//! [`spawn_event_gate`]:
//!
//! - **Gate 1 — per-node admission** at `ClientConnected`: accept iff the
//!   QUIC/TLS-authenticated remote `EndpointId` resolves to an Active member;
//!   else `AbortReason::Permission`.
//! - **Gate 2 — per-hash authorization** at `GetRequestReceived` /
//!   `GetManyRequestReceived`: serve a hash only if it is referenced by a valid
//!   `file.shared`; else `AbortReason::Permission`.
//!
//! Push and observe requests are denied outright (the room never lets a peer
//! write to or enumerate the provider store over the blobs ALPN).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::endpoint::{ApplicationClose, Connection, ConnectionError};
use iroh::{
    endpoint::presets, protocol::Router, Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey,
};
use iroh_blobs::get::request::get_blob;
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ProviderMessage, RequestMode,
};
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::{BlobsProtocol, Hash};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::acl::AuthContext;

/// Classified result of a single fetch attempt — the spike's observable outcome
/// for each row of the §5.5 decision matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Transfer completed and the receiver's BLAKE3 recheck matched the declared
    /// `file.shared.blob_hash`.
    Fetched,
    /// Denied at the connect gate (Gate 1): the peer is not an Active member.
    DeniedAtConnect,
    /// Denied at the per-hash gate (Gate 2): the hash is not referenced.
    DeniedPerHash,
    /// Transfer completed but the receiver's BLAKE3 recheck did NOT match the
    /// declared hash (a `file.shared` that lies about its content).
    HashMismatch,
    /// No provider served the hash within the timeout (offline / never imported).
    Unavailable,
}

/// A running, ACL-gated blob provider.
pub struct Provider {
    router: Router,
    store: MemStore,
    gate: JoinHandle<()>,
}

impl Provider {
    /// Stand up an in-memory `iroh-blobs` provider bound to `secret`, gated by
    /// `auth`. The endpoint is local-only (relay disabled) so the spike runs
    /// fully offline over loopback.
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind.
    pub async fn spawn(secret: SecretKey, auth: AuthContext) -> Result<Self> {
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .context("bind provider endpoint")?;

        let store = MemStore::new();
        let (events, gate) = spawn_event_gate(auth);
        let blobs = BlobsProtocol::new(&store, Some(events));
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs)
            .spawn();

        Ok(Self {
            router,
            store,
            gate,
        })
    }

    /// The provider's authenticated identity (`EndpointId` == `device_id`).
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.router.endpoint().id()
    }

    /// Import a blob into the store and return its content hash. Asserts the
    /// store's hash equals an independent BLAKE3-256 of the same bytes (PRD §9.2
    /// content-addressing step).
    ///
    /// # Errors
    /// Returns an error if the import fails or the store hash disagrees with the
    /// independent BLAKE3 recompute.
    pub async fn import(&self, data: impl Into<Bytes>) -> Result<Hash> {
        let data = data.into();
        let expected = Hash::from(*blake3::hash(&data).as_bytes());
        let tag = self
            .store
            .blobs()
            .add_bytes(data)
            .await
            .context("import blob")?;
        anyhow::ensure!(
            tag.hash == expected,
            "store import hash {} != independent blake3 {}",
            tag.hash,
            expected,
        );
        Ok(tag.hash)
    }

    /// A loopback dialing address for this provider (id + `127.0.0.1:<bound
    /// port>`), so a fetcher can reach it without relay or DNS discovery.
    ///
    /// # Errors
    /// Returns an error if the endpoint has no bound UDP socket.
    pub fn dial_addr(&self) -> Result<EndpointAddr> {
        loopback_addr(self.router.endpoint())
    }

    /// Gracefully stop the provider (used by the "unavailable" scenario).
    ///
    /// # Errors
    /// Returns an error if the router fails to shut down.
    pub async fn shutdown(self) -> Result<()> {
        self.gate.abort();
        self.router.shutdown().await.context("shutdown router")?;
        Ok(())
    }
}

/// Bind a fresh, local-only fetcher endpoint with the given identity.
///
/// # Errors
/// Returns an error if the endpoint fails to bind.
pub async fn bind_fetcher(secret: SecretKey) -> Result<Endpoint> {
    Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .context("bind fetcher endpoint")
}

/// Drive the `iroh-blobs` provider-events channel as the two-gate ACL.
///
/// Returns the [`EventSender`] to hand to [`BlobsProtocol::new`] and the task
/// handle running the decision loop. The task ends when the provider (and thus
/// the event channel) is dropped.
#[must_use]
pub fn spawn_event_gate(auth: AuthContext) -> (EventSender, JoinHandle<()>) {
    // Gate 1 via `connected = Intercept`; Gate 2 via `get`/`get_many =
    // Intercept`. Push stays Disabled (no writes), observe stays None.
    let mask = EventMask {
        connected: ConnectMode::Intercept,
        get: RequestMode::Intercept,
        get_many: RequestMode::Intercept,
        ..EventMask::DEFAULT
    };
    let (events, mut rx) = EventSender::channel(64, mask);

    let handle = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                // Gate 1 — per-node admission.
                ProviderMessage::ClientConnected(msg) => {
                    let decision = match msg.endpoint_id {
                        Some(id) if auth.is_active(id) => {
                            info!(peer = %id, "gate1: accept (active member)");
                            Ok(())
                        }
                        Some(id) => {
                            warn!(peer = %id, "gate1: reject (not an active member)");
                            Err(AbortReason::Permission)
                        }
                        None => {
                            warn!("gate1: reject (unauthenticated peer)");
                            Err(AbortReason::Permission)
                        }
                    };
                    msg.tx.send(decision).await.ok();
                }
                // Gate 2 — per-hash authorization (single blob).
                ProviderMessage::GetRequestReceived(msg) => {
                    let hash = msg.request.hash;
                    let decision = if auth.is_referenced(&hash) {
                        info!(%hash, "gate2: serve (referenced)");
                        Ok(())
                    } else {
                        warn!(%hash, "gate2: Permission (unreferenced hash)");
                        Err(AbortReason::Permission)
                    };
                    msg.tx.send(decision).await.ok();
                }
                // Gate 2 — per-hash authorization (hash_seq / multi). Deny unless
                // every requested hash is referenced.
                ProviderMessage::GetManyRequestReceived(msg) => {
                    let all_referenced = msg.request.hashes.iter().all(|h| auth.is_referenced(h));
                    let decision = if all_referenced {
                        Ok(())
                    } else {
                        warn!("gate2: Permission (unreferenced hash in get_many)");
                        Err(AbortReason::Permission)
                    };
                    msg.tx.send(decision).await.ok();
                }
                // The room never permits writing to or enumerating the store
                // over the blobs ALPN.
                ProviderMessage::PushRequestReceived(msg) => {
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::ObserveRequestReceived(msg) => {
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

/// Fetch `fetch_hash` from `provider_addr`, then independently verify the
/// assembled bytes against `declared_hash` (the `file.shared.blob_hash`).
///
/// The whole attempt is wrapped in `timeout` so the unavailable path fails
/// promptly with a distinct [`FetchOutcome::Unavailable`] instead of hanging.
///
/// `fetch_hash` is what is requested on the wire; `declared_hash` is what the
/// receiver checks against. They differ only in the tamper scenario, where a
/// `file.shared` declares a hash that does not match the bytes it points at.
pub async fn fetch_and_verify(
    fetcher: &Endpoint,
    provider_addr: EndpointAddr,
    fetch_hash: Hash,
    declared_hash: [u8; 32],
    timeout: Duration,
) -> (FetchOutcome, Option<Bytes>) {
    let connect = fetcher.connect(provider_addr, iroh_blobs::ALPN);
    let conn = match tokio::time::timeout(timeout, connect).await {
        Err(_elapsed) => {
            warn!("fetch: connect timed out -> Unavailable");
            return (FetchOutcome::Unavailable, None);
        }
        Ok(Err(err)) => {
            // Provider unreachable. The connect gate closes *after* the
            // handshake, so a denied member still connects here and is caught at
            // the get step below; a hard connect error means nobody is serving.
            warn!(%err, "fetch: connect failed -> Unavailable");
            return (FetchOutcome::Unavailable, None);
        }
        Ok(Ok(conn)) => conn,
    };

    // Keep a handle to inspect the connection's close reason: the connect gate
    // (Gate 1) closes the whole connection, which can surface on the getter as a
    // connection-level error rather than a stream reset.
    let probe = conn.clone();
    let get = get_blob(conn, fetch_hash);
    match tokio::time::timeout(timeout, get.bytes()).await {
        Err(_elapsed) => {
            warn!("fetch: transfer timed out -> Unavailable");
            (FetchOutcome::Unavailable, None)
        }
        Ok(Err(err)) => {
            let outcome = classify_get_failure(&err, &probe).await;
            warn!(%err, ?outcome, "fetch: get failed");
            (outcome, None)
        }
        Ok(Ok(bytes)) => {
            // AC4 — independent receiver-side content verification.
            let actual = blake3::hash(&bytes);
            if actual.as_bytes() == &declared_hash {
                info!(len = bytes.len(), "fetch: verified -> Fetched");
                (FetchOutcome::Fetched, Some(bytes))
            } else {
                warn!("fetch: BLAKE3 != declared hash -> HashMismatch");
                (FetchOutcome::HashMismatch, Some(bytes))
            }
        }
    }
}

/// Map an `iroh-blobs` getter failure to a [`FetchOutcome`].
///
/// Both gates abort with `ERR_PERMISSION`, but they fail at different points:
///
/// - **Gate 2 (per-hash)** resets the *response stream*, which `iroh-blobs`
///   surfaces as a `ReadError::Reset(ERR_PERMISSION)` decodable from
///   [`GetError::iroh_error_code`].
/// - **Gate 1 (connect)** closes the *whole connection*. Depending on timing the
///   getter sees this either when opening the stream or while reading, and the
///   close code is not always recoverable from the `GetError` alone — so we also
///   consult the connection's close reason.
async fn classify_get_failure(err: &iroh_blobs::get::GetError, conn: &Connection) -> FetchOutcome {
    match classify_get_error(err) {
        // Definitive stream-level classifications stand on their own.
        FetchOutcome::DeniedPerHash => FetchOutcome::DeniedPerHash,
        FetchOutcome::DeniedAtConnect => FetchOutcome::DeniedAtConnect,
        // Ambiguous: did the connect gate close the connection, or is the
        // provider genuinely unavailable? The close reason disambiguates.
        _ => {
            if connection_denied_for_permission(conn).await {
                FetchOutcome::DeniedAtConnect
            } else {
                FetchOutcome::Unavailable
            }
        }
    }
}

/// Stream-level classification from the `GetError` alone.
fn classify_get_error(err: &iroh_blobs::get::GetError) -> FetchOutcome {
    let is_permission = err.iroh_error_code() == Some(iroh_blobs::protocol::ERR_PERMISSION);
    if is_permission && err.open().is_some() {
        FetchOutcome::DeniedAtConnect
    } else if is_permission && (err.remote_read().is_some() || err.remote_write().is_some()) {
        FetchOutcome::DeniedPerHash
    } else {
        FetchOutcome::Unavailable
    }
}

/// Was the connection closed with `ERR_PERMISSION` (the connect gate)? If the
/// close has not yet been observed, wait briefly for it — a genuinely Active
/// peer's per-hash denial leaves the connection open, so this short wait only
/// applies to the connect-deny path (the offline path fails at `connect()`).
async fn connection_denied_for_permission(conn: &Connection) -> bool {
    let reason = match conn.close_reason() {
        Some(reason) => Some(reason),
        None => tokio::time::timeout(Duration::from_millis(500), conn.closed())
            .await
            .ok(),
    };
    matches!(
        reason,
        Some(ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. }))
            if error_code == iroh_blobs::protocol::ERR_PERMISSION
    )
}

/// Build a loopback [`EndpointAddr`] (id + `127.0.0.1:<bound port>`) for a local
/// endpoint, bypassing relay/DNS discovery.
fn loopback_addr(endpoint: &Endpoint) -> Result<EndpointAddr> {
    let port = endpoint
        .bound_sockets()
        .into_iter()
        .map(|s| s.port())
        .next()
        .context("endpoint has no bound UDP socket")?;
    let socket = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    Ok(EndpointAddr::new(endpoint.id()).with_ip_addr(socket))
}
