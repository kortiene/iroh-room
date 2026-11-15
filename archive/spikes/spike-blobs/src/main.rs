//! Narrated demo for the IR-0009 blob ACL spike.
//!
//! Stands up one ACL-gated `iroh-blobs` provider (Bob) and drives every row of
//! the §5.5 / Test Vector §16 decision matrix as a distinct fetcher identity,
//! printing each gate decision and the final outcome. Exits non-zero if any
//! observed outcome diverges from the expected one, so the demo doubles as a
//! self-checking confirmation (`scripts/verify.sh` and manual runs alike).
//!
//! Run with `RUST_LOG=info cargo run -p spike-blobs` to see the gate logs.

use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::EndpointId;
use iroh_blobs::Hash;
use spike_blobs::file_shared::FileShared;
use spike_blobs::net::{bind_fetcher, fetch_and_verify, FetchOutcome, Provider};
use spike_blobs::roster;
use tracing::info;

/// Bounded per-fetch timeout. Generous enough for loopback transfer, short
/// enough that the unavailable path reports promptly instead of hanging.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Shorter timeout for the deliberately-unavailable scenario.
const UNAVAILABLE_TIMEOUT: Duration = Duration::from_secs(3);

/// The provider plus everything the scenarios derive from the imported blobs and
/// the create/consume of the `file.shared` reference.
struct Demo {
    provider: Provider,
    provider_addr: iroh::EndpointAddr,
    referenced_hash: Hash,
    unreferenced_hash: Hash,
    /// `file.shared.blob_hash` as consumed by the fetcher (AC6).
    declared_hash: [u8; 32],
    /// A hash that does NOT match the served bytes (tampered reference, AC4).
    tampered_hash: [u8; 32],
}

/// Stand up the provider, import the referenced + unreferenced blobs, and create
/// then consume the `file.shared` reference (AC6).
async fn setup() -> Result<Demo> {
    let shared_bytes = Bytes::from_static(b"hello room, this is a shared artifact\n");
    let other_bytes = Bytes::from_static(b"a different, unreferenced blob in the same store\n");

    // Provider (Bob): import the shared blob and a second, UNREFERENCED blob.
    let referenced_hash_probe = Hash::from(*blake3::hash(&shared_bytes).as_bytes());
    let auth = roster::test_vector_auth(&[referenced_hash_probe]);
    let provider = Provider::spawn(roster::secret(roster::BOB), auth).await?;

    let referenced_hash = provider.import(shared_bytes.clone()).await?;
    let unreferenced_hash = provider.import(other_bytes).await?;
    anyhow::ensure!(
        referenced_hash == referenced_hash_probe,
        "hash probe mismatch"
    );
    info!(%referenced_hash, %unreferenced_hash, provider = %provider.id(), "provider ready");

    // Create + consume the `file.shared` reference (AC6).
    let file_shared = FileShared::new_raw(
        "file-0001",
        "artifact.txt",
        "text/plain",
        shared_bytes.len() as u64,
        *referenced_hash.as_bytes(),
        &[*provider.id().as_bytes()],
    );
    let encoded = file_shared.encode().context("encode file.shared")?;
    let consumed = FileShared::decode(&encoded).context("decode file.shared")?;
    let declared_hash = consumed.blob_hash_array().context("declared blob_hash")?;
    let declared_provider = consumed
        .provider_arrays()
        .context("declared providers")?
        .into_iter()
        .next()
        .context("file.shared has no provider")?;
    let declared_provider_id =
        EndpointId::from_bytes(&declared_provider).context("provider EndpointId")?;
    anyhow::ensure!(
        declared_provider_id == provider.id(),
        "consumed provider id does not match the actual provider",
    );
    anyhow::ensure!(
        Hash::from(declared_hash) == referenced_hash,
        "consumed blob_hash does not match the imported hash",
    );
    info!("file.shared created and consumed: fetcher learned blob_hash + provider");

    let provider_addr = provider.dial_addr()?;
    let tampered_hash = {
        let mut bytes = *referenced_hash.as_bytes();
        bytes[0] ^= 0xFF; // declare a hash the served bytes will not match
        bytes
    };

    Ok(Demo {
        provider,
        provider_addr,
        referenced_hash,
        unreferenced_hash,
        declared_hash,
        tampered_hash,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let Demo {
        provider,
        provider_addr,
        referenced_hash,
        unreferenced_hash,
        declared_hash,
        tampered_hash,
    } = setup().await?;

    let mut results: Vec<Scenario> = Vec::new();

    // 1. Authorized fetch — Carol (Active) fetches the referenced blob.
    results.push(
        run(
            "AC1 authorized member",
            roster::CAROL,
            provider_addr.clone(),
            referenced_hash,
            declared_hash,
            FETCH_TIMEOUT,
            FetchOutcome::Fetched,
        )
        .await?,
    );

    // 2. Non-member at connect — Mallory (unknown) denied.
    results.push(
        run(
            "AC2 non-member",
            roster::MALLORY,
            provider_addr.clone(),
            referenced_hash,
            declared_hash,
            FETCH_TIMEOUT,
            FetchOutcome::DeniedAtConnect,
        )
        .await?,
    );

    // 3. Removed member at connect — Dave (Removed) denied.
    results.push(
        run(
            "AC2 removed member",
            roster::DAVE,
            provider_addr.clone(),
            referenced_hash,
            declared_hash,
            FETCH_TIMEOUT,
            FetchOutcome::DeniedAtConnect,
        )
        .await?,
    );

    // 4. Unreferenced hash — Carol (Active) denied per-hash.
    results.push(
        run(
            "AC3 unreferenced hash",
            roster::CAROL,
            provider_addr.clone(),
            unreferenced_hash,
            *unreferenced_hash.as_bytes(),
            FETCH_TIMEOUT,
            FetchOutcome::DeniedPerHash,
        )
        .await?,
    );

    // 5. Tampered reference — bytes verify against a declared hash they don't match.
    results.push(
        run(
            "AC4 tampered hash",
            roster::CAROL,
            provider_addr.clone(),
            referenced_hash,
            tampered_hash,
            FETCH_TIMEOUT,
            FetchOutcome::HashMismatch,
        )
        .await?,
    );

    // 6. Unavailable provider — shut the provider down, then fetch.
    info!("shutting provider down for the unavailable scenario");
    provider.shutdown().await?;
    results.push(
        run(
            "AC7 unavailable provider",
            roster::CAROL,
            provider_addr,
            referenced_hash,
            declared_hash,
            UNAVAILABLE_TIMEOUT,
            FetchOutcome::Unavailable,
        )
        .await?,
    );

    print_summary(&results);
    let failures = results.iter().filter(|s| !s.passed()).count();
    anyhow::ensure!(
        failures == 0,
        "{failures} scenario(s) did not match expectations"
    );
    println!(
        "\nAll {} scenarios matched expectations. GATE: GO.",
        results.len()
    );
    Ok(())
}

/// One row of the decision matrix and its observed result.
struct Scenario {
    label: &'static str,
    expected: FetchOutcome,
    actual: FetchOutcome,
}

impl Scenario {
    fn passed(&self) -> bool {
        self.expected == self.actual
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    label: &'static str,
    fetcher_seed: u8,
    provider_addr: iroh::EndpointAddr,
    fetch_hash: Hash,
    declared_hash: [u8; 32],
    timeout: Duration,
    expected: FetchOutcome,
) -> Result<Scenario> {
    let fetcher = bind_fetcher(roster::secret(fetcher_seed)).await?;
    info!(scenario = label, fetcher = %fetcher.id(), "--- running scenario");
    let (actual, _bytes) =
        fetch_and_verify(&fetcher, provider_addr, fetch_hash, declared_hash, timeout).await;
    fetcher.close().await;
    info!(scenario = label, ?expected, ?actual, "scenario complete");
    Ok(Scenario {
        label,
        expected,
        actual,
    })
}

fn print_summary(results: &[Scenario]) {
    println!("\n=== Blob ACL spike — decision matrix ===");
    for s in results {
        let mark = if s.passed() { "ok" } else { "FAIL" };
        println!(
            "  [{mark}] {label:28} expected {expected:?}, got {actual:?}",
            mark = mark,
            label = s.label,
            expected = s.expected,
            actual = s.actual,
        );
    }
}
