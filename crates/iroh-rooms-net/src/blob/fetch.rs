//! The verified fetch client (IR-0204 spec §5.4): dial a provider over the blobs
//! ALPN, transfer via `iroh-blobs`' BLAKE3 bao-verified streaming, then
//! independently recompute BLAKE3-256 over the assembled bytes and require it
//! equals the caller's declared hash. Lifted from `spike-blobs::net::{
//! fetch_and_verify, classify_get_failure, classify_get_error,
//! connection_denied_for_permission}`.
//!
//! `iroh-blobs` bao verified streaming already rejects bytes that do not match
//! the *requested* hash during transfer; the independent recompute here guards a
//! different thing — a `file.shared` reference that *declares* a hash different
//! from the content it actually points at (spike §5.3 / NOTES.md §4).

use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{ApplicationClose, Connection, ConnectionError};
use iroh::{Endpoint, EndpointAddr};
use iroh_blobs::get::request::get_blob;
use iroh_blobs::get::GetError;
use iroh_blobs::Hash;

/// Classified outcome of one fetch attempt against one provider (the issue's
/// acceptance criteria / spec §5.5 decision matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Transfer completed and the receiver's independent BLAKE3 recheck matched
    /// the declared hash (AC1/AC4).
    Fetched,
    /// Denied at the connect gate (Gate 1): the peer is not an active member (AC3).
    DeniedAtConnect,
    /// Denied at the per-hash gate (Gate 2): the hash is not referenced (AC3).
    DeniedPerHash,
    /// Transfer completed but the receiver's BLAKE3 recheck did NOT match the
    /// declared hash — a `file.shared` that lies about its content (AC2).
    HashMismatch,
    /// No provider served the hash within `timeout` (offline / never imported;
    /// AC5 — honest unavailable, never a hang).
    Unavailable,
}

/// Fetch `fetch_hash` from `provider_addr` over the blobs ALPN using `endpoint`,
/// then independently verify the assembled bytes against `declared_hash`. The
/// whole attempt (connect + transfer) is bounded by `timeout`, so an offline or
/// non-holding provider yields [`FetchOutcome::Unavailable`], never a hang.
///
/// `fetch_hash` is what is requested on the wire; `declared_hash` is what the
/// receiver checks the result against — they differ only when a `file.shared`
/// declares a hash that does not match the bytes it references.
pub async fn fetch_blob(
    endpoint: &Endpoint,
    provider_addr: EndpointAddr,
    fetch_hash: [u8; 32],
    declared_hash: [u8; 32],
    timeout: Duration,
) -> (FetchOutcome, Option<Bytes>) {
    let connect = endpoint.connect(provider_addr, iroh_blobs::ALPN);
    let conn = match tokio::time::timeout(timeout, connect).await {
        Err(_elapsed) => {
            tracing::debug!("blob fetch: connect timed out -> Unavailable");
            return (FetchOutcome::Unavailable, None);
        }
        Ok(Err(err)) => {
            // The connect gate closes *after* the handshake, so a denied member
            // still connects here and is caught at the get step below; a hard
            // connect error means nobody is serving.
            tracing::debug!(%err, "blob fetch: connect failed -> Unavailable");
            return (FetchOutcome::Unavailable, None);
        }
        Ok(Ok(conn)) => conn,
    };

    // Keep a handle to inspect the connection's close reason: the connect gate
    // (Gate 1) closes the whole connection, which can surface on the getter as a
    // connection-level error rather than a stream reset.
    let probe = conn.clone();
    let get = get_blob(conn, Hash::from(fetch_hash));
    match tokio::time::timeout(timeout, get.bytes()).await {
        Err(_elapsed) => {
            tracing::debug!("blob fetch: transfer timed out -> Unavailable");
            (FetchOutcome::Unavailable, None)
        }
        Ok(Err(err)) => {
            let outcome = classify_get_failure(&err, &probe).await;
            tracing::debug!(%err, ?outcome, "blob fetch: get failed");
            (outcome, None)
        }
        Ok(Ok(bytes)) => {
            // AC2 — independent receiver-side content verification.
            let actual = blake3::hash(&bytes);
            if actual.as_bytes() == &declared_hash {
                (FetchOutcome::Fetched, Some(bytes))
            } else {
                (FetchOutcome::HashMismatch, Some(bytes))
            }
        }
    }
}

/// Map an `iroh-blobs` getter failure to a [`FetchOutcome`].
///
/// Both gates abort with `ERR_PERMISSION`, but they fail at different points:
/// Gate 2 (per-hash) resets the *response stream*; Gate 1 (connect) closes the
/// *whole connection*, so an ambiguous stream-level classification is
/// disambiguated against the connection's close reason.
async fn classify_get_failure(err: &GetError, conn: &Connection) -> FetchOutcome {
    match classify_get_error(err) {
        FetchOutcome::DeniedPerHash => FetchOutcome::DeniedPerHash,
        FetchOutcome::DeniedAtConnect => FetchOutcome::DeniedAtConnect,
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
fn classify_get_error(err: &GetError) -> FetchOutcome {
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
/// close has not yet been observed, wait briefly for it — a genuinely active
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

#[cfg(test)]
mod tests {
    use super::{classify_get_error, FetchOutcome};

    #[test]
    fn fetch_outcome_variants_are_distinct() {
        assert_ne!(FetchOutcome::Fetched, FetchOutcome::DeniedAtConnect);
        assert_ne!(FetchOutcome::DeniedAtConnect, FetchOutcome::DeniedPerHash);
        assert_ne!(FetchOutcome::DeniedPerHash, FetchOutcome::HashMismatch);
        assert_ne!(FetchOutcome::HashMismatch, FetchOutcome::Unavailable);
    }

    // `classify_get_error` needs a live `GetError` to exercise the open/
    // remote_read/remote_write branches, which only `iroh-blobs`' own getter
    // machinery can construct; that mapping is exercised end-to-end by the
    // always-green Node-level `blob_e2e` integration suite. This test only pins
    // that the function exists with the documented signature via a type check.
    #[test]
    fn classify_get_error_has_expected_signature() {
        fn assert_signature(f: fn(&iroh_blobs::get::GetError) -> FetchOutcome) {
            let _ = f;
        }
        assert_signature(classify_get_error);
    }
}
