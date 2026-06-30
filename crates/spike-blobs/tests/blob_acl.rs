//! Automated scenarios for the blob ACL gate (Test Vectors §16 / §5.5 matrix).
//!
//! Each test stands up two in-process iroh endpoints over loopback (relay
//! disabled), so the suite runs offline. They map to the issue's acceptance
//! criteria: authorized member (AC1), unauthorized peers (AC2), unreferenced
//! hash (AC3), receiver verification (AC4), and the unavailable provider state
//! (AC7). Both gates denying correctly is the Day-8 soft GATE (AC8).

use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use iroh_blobs::Hash;
use spike_blobs::file_shared::FileShared;
use spike_blobs::net::{bind_fetcher, fetch_and_verify, FetchOutcome, Provider};
use spike_blobs::roster;

const TIMEOUT: Duration = Duration::from_secs(10);
const SHORT_TIMEOUT: Duration = Duration::from_secs(3);

const SHARED: &[u8] = b"shared artifact bytes for the room\n";
const OTHER: &[u8] = b"a second, unreferenced blob in the same store\n";

/// Provider (Bob) holding a referenced blob and a second UNREFERENCED blob, with
/// a Test Vector §16 authorization snapshot referencing only the first.
async fn provider_with_blobs() -> Result<(Provider, Hash, Hash)> {
    let referenced = Hash::from(*blake3::hash(SHARED).as_bytes());
    let auth = roster::test_vector_auth(&[referenced]);
    let provider = Provider::spawn(roster::secret(roster::BOB), auth).await?;
    let r = provider.import(Bytes::from_static(SHARED)).await?;
    let u = provider.import(Bytes::from_static(OTHER)).await?;
    assert_eq!(r, referenced, "import hash must equal independent BLAKE3");
    Ok((provider, r, u))
}

/// AC1 — an Active member fetches a referenced blob and the receiver verifies it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_member_fetches_referenced_blob() -> Result<()> {
    let (provider, referenced, _unreferenced) = provider_with_blobs().await?;
    let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;

    let (outcome, bytes) = fetch_and_verify(
        &carol,
        provider.dial_addr()?,
        referenced,
        *referenced.as_bytes(),
        TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::Fetched);
    assert_eq!(bytes.as_deref(), Some(SHARED));
    provider.shutdown().await?;
    Ok(())
}

/// AC2 — a non-member (Mallory, unknown identity) is denied at the connect gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_member_denied_at_connect() -> Result<()> {
    let (provider, referenced, _u) = provider_with_blobs().await?;
    let mallory = bind_fetcher(roster::secret(roster::MALLORY)).await?;

    let (outcome, _bytes) = fetch_and_verify(
        &mallory,
        provider.dial_addr()?,
        referenced,
        *referenced.as_bytes(),
        TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::DeniedAtConnect);
    provider.shutdown().await?;
    Ok(())
}

/// AC2 — a removed member (Dave, bound but not Active) is denied at the connect gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removed_member_denied_at_connect() -> Result<()> {
    let (provider, referenced, _u) = provider_with_blobs().await?;
    let dave = bind_fetcher(roster::secret(roster::DAVE)).await?;

    let (outcome, _bytes) = fetch_and_verify(
        &dave,
        provider.dial_addr()?,
        referenced,
        *referenced.as_bytes(),
        TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::DeniedAtConnect);
    provider.shutdown().await?;
    Ok(())
}

/// AC3 — an Active member cannot fetch an unreferenced hash (per-hash gate),
/// proving Gate 2 is independent of node admission (AC8 soft GATE).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_member_denied_unreferenced_hash() -> Result<()> {
    let (provider, _referenced, unreferenced) = provider_with_blobs().await?;
    let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;

    let (outcome, _bytes) = fetch_and_verify(
        &carol,
        provider.dial_addr()?,
        unreferenced,
        *unreferenced.as_bytes(),
        TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::DeniedPerHash);
    provider.shutdown().await?;
    Ok(())
}

/// AC4 — the receiver rejects bytes that do not match the declared hash
/// (a `file.shared` that lies about its content).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receiver_rejects_hash_mismatch() -> Result<()> {
    let (provider, referenced, _u) = provider_with_blobs().await?;
    let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;

    let mut tampered = *referenced.as_bytes();
    tampered[0] ^= 0xFF; // declare a hash the served bytes will not match

    let (outcome, _bytes) =
        fetch_and_verify(&carol, provider.dial_addr()?, referenced, tampered, TIMEOUT).await;

    assert_eq!(outcome, FetchOutcome::HashMismatch);
    provider.shutdown().await?;
    Ok(())
}

/// AC7 — with no provider online, the fetch fails cleanly within the timeout and
/// reports a distinct Unavailable outcome (no hang, no panic).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_provider_reported_cleanly() -> Result<()> {
    let (provider, referenced, _u) = provider_with_blobs().await?;
    let addr = provider.dial_addr()?;
    provider.shutdown().await?; // take the provider offline

    let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;
    let (outcome, _bytes) = fetch_and_verify(
        &carol,
        addr,
        referenced,
        *referenced.as_bytes(),
        SHORT_TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::Unavailable);
    Ok(())
}

/// AC1 (admin role) — Alice (Active admin) can also fetch a referenced blob.
/// Confirms the fixture correctly counts admin as Active (not a special gate path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_member_fetches_referenced_blob() -> Result<()> {
    let (provider, referenced, _unreferenced) = provider_with_blobs().await?;
    let alice = bind_fetcher(roster::secret(roster::ALICE)).await?;

    let (outcome, bytes) = fetch_and_verify(
        &alice,
        provider.dial_addr()?,
        referenced,
        *referenced.as_bytes(),
        TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::Fetched);
    assert_eq!(bytes.as_deref(), Some(SHARED));
    provider.shutdown().await?;
    Ok(())
}

/// AC6 — provider creates a `file.shared` payload; fetcher consumes it to derive
/// `declared_hash` and drives `fetch_and_verify`. This is the explicit
/// create+consume→fetch integration path (not only exercised by the demo binary).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_shared_create_and_consume_drives_fetch() -> Result<()> {
    let (provider, referenced, _unreferenced) = provider_with_blobs().await?;
    let provider_id_bytes = *provider.id().as_bytes();
    let addr = provider.dial_addr()?;

    // Provider creates the file.shared reference after import.
    let file_shared = FileShared::new_raw(
        "test-file-001",
        "artifact.bin",
        "application/octet-stream",
        SHARED.len() as u64,
        *referenced.as_bytes(),
        &[provider_id_bytes],
    );
    let encoded = file_shared.encode().expect("encode file.shared");

    // Fetcher consumes the payload to learn blob_hash.
    let consumed = FileShared::decode(&encoded).expect("decode file.shared");
    let declared_hash = consumed.blob_hash_array().expect("blob_hash");

    let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;
    let (outcome, bytes) = fetch_and_verify(&carol, addr, referenced, declared_hash, TIMEOUT).await;

    assert_eq!(outcome, FetchOutcome::Fetched);
    assert_eq!(bytes.as_deref(), Some(SHARED));
    provider.shutdown().await?;
    Ok(())
}

/// AC8 gate-ordering — Gate 1 fires before Gate 2: a non-member requesting an
/// unreferenced hash is denied at connect (not per-hash), confirming the gates
/// are independent and ordered correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_member_denied_at_connect_takes_priority_over_per_hash_gate() -> Result<()> {
    let (provider, _referenced, unreferenced) = provider_with_blobs().await?;
    let mallory = bind_fetcher(roster::secret(roster::MALLORY)).await?;

    // Mallory requests an unreferenced hash — Gate 1 must fire before Gate 2.
    let (outcome, _bytes) = fetch_and_verify(
        &mallory,
        provider.dial_addr()?,
        unreferenced,
        *unreferenced.as_bytes(),
        TIMEOUT,
    )
    .await;

    assert_eq!(outcome, FetchOutcome::DeniedAtConnect);
    provider.shutdown().await?;
    Ok(())
}

// ── Additional e2e coverage (edge cases, concurrency) ──────────────────────────
//
// NOTE — provider-as-fetcher (same identity): connecting a second Endpoint built
// from the same SecretKey (the provider's identity) back to the provider returns
// Unavailable in iroh 1.0. This is an iroh transport-layer restriction (same-
// EndpointId connections are rejected), not an ACL issue. The ACL gate is
// identity-agnostic; the provider's identity receives no special treatment in
// `AuthContext`. This observation is recorded in NOTES.md §8.

/// PRD §14 / §18.2 — a hash that IS in the ACL allowlist (Gate 2 passes) but is
/// NOT present in the provider's blob store must surface as `Unavailable`, not as
/// `DeniedPerHash`. This distinguishes "ACL-denied" from "content unavailable"
/// and ensures Gate 2 authorization does not imply store presence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn referenced_hash_not_in_store_surfaces_unavailable() -> Result<()> {
    // phantom_hash is added to the ACL's referenced set but never imported.
    let phantom_hash = Hash::from([0xFE; 32]);
    let auth = roster::test_vector_auth(&[phantom_hash]);
    let provider = Provider::spawn(roster::secret(roster::BOB), auth).await?;
    // Import an unrelated blob so the store is non-empty but lacks phantom_hash.
    let _ = provider
        .import(Bytes::from_static(b"unrelated blob data"))
        .await?;

    let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;
    let (outcome, _bytes) = fetch_and_verify(
        &carol,
        provider.dial_addr()?,
        phantom_hash,
        *phantom_hash.as_bytes(),
        SHORT_TIMEOUT,
    )
    .await;

    // Gate 1 passes (Carol is Active), Gate 2 passes (phantom_hash is referenced),
    // but the store has nothing to serve → outcome must be Unavailable.
    assert_eq!(outcome, FetchOutcome::Unavailable);
    provider.shutdown().await?;
    Ok(())
}

/// Concurrent gate operation — an Active member (Carol) and a non-member (Mallory)
/// fetch simultaneously. The gate must correctly admit Carol and deny Mallory
/// without deadlocks, panics, or cross-request confusion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_active_and_denied_fetches_both_gate_correctly() -> Result<()> {
    let (provider, referenced, _unreferenced) = provider_with_blobs().await?;
    let carol_addr = provider.dial_addr()?;
    let mallory_addr = carol_addr.clone();

    let (carol_result, mallory_result) = tokio::join!(
        async {
            let carol = bind_fetcher(roster::secret(roster::CAROL)).await?;
            let (outcome, _) = fetch_and_verify(
                &carol,
                carol_addr,
                referenced,
                *referenced.as_bytes(),
                TIMEOUT,
            )
            .await;
            anyhow::Ok(outcome)
        },
        async {
            let mallory = bind_fetcher(roster::secret(roster::MALLORY)).await?;
            let (outcome, _) = fetch_and_verify(
                &mallory,
                mallory_addr,
                referenced,
                *referenced.as_bytes(),
                TIMEOUT,
            )
            .await;
            anyhow::Ok(outcome)
        },
    );

    assert_eq!(
        carol_result?,
        FetchOutcome::Fetched,
        "Active member Carol must succeed even under concurrent load"
    );
    assert_eq!(
        mallory_result?,
        FetchOutcome::DeniedAtConnect,
        "Non-member Mallory must be denied even under concurrent load"
    );
    provider.shutdown().await?;
    Ok(())
}
