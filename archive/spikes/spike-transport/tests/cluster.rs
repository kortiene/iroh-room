//! Focused, endpoint-free coverage of the generic [`Cluster`] harness logic
//! (`lib.rs` §7.1): convergence detection, the deadline exit, and publish
//! routing. `tests/self_check.rs` exercises this through *real* loopback
//! endpoints (mesh/gossip); those runs cannot isolate the polling loop's
//! behavior (immediate convergence vs. mid-flight arrival vs. never-converges)
//! or prove it returns the exact per-node sets a caller diffs for the failure
//! ledger. A trivial in-memory [`Mock`] backend — no sockets, no I/O — pins
//! that generic logic deterministically and fast.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use iroh_rooms_core::event::EventId;
use spike_transport::{BackendEvent, BackendKind, Cluster, TransportBackend, WireBytes};

/// A deterministic `EventId` from a single fill byte (no endpoints, no hashing
/// of real events needed — the oracle only ever compares ids by value).
fn eid(fill: u8) -> EventId {
    EventId::from_bytes([fill; 32])
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A pure in-memory [`TransportBackend`]: `received_ids()` returns whatever has
/// been seeded/inserted, `publish()` just records the bytes it was handed. No
/// network, so the [`Cluster`] logic under test is the only thing exercised.
struct Mock {
    kind: BackendKind,
    received: Mutex<BTreeSet<EventId>>,
    published: Mutex<Vec<WireBytes>>,
}

impl Mock {
    fn new(kind: BackendKind) -> Self {
        Self {
            kind,
            received: Mutex::new(BTreeSet::new()),
            published: Mutex::new(Vec::new()),
        }
    }

    fn with_ids(kind: BackendKind, ids: BTreeSet<EventId>) -> Self {
        Self {
            kind,
            received: Mutex::new(ids),
            published: Mutex::new(Vec::new()),
        }
    }

    /// Merge `ids` into this node's received set (simulates events arriving).
    fn insert_all(&self, ids: &BTreeSet<EventId>) {
        lock(&self.received).extend(ids.iter().copied());
    }

    fn published(&self) -> Vec<WireBytes> {
        lock(&self.published).clone()
    }
}

#[async_trait]
impl TransportBackend for Mock {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    async fn publish(&self, wire: WireBytes) -> anyhow::Result<()> {
        lock(&self.published).push(wire);
        Ok(())
    }

    fn received_ids(&self) -> BTreeSet<EventId> {
        lock(&self.received).clone()
    }

    fn drain_events(&self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

#[test]
fn empty_cluster_reports_empty() {
    let cluster: Cluster<Mock> = Cluster::new(Vec::new());
    assert!(cluster.is_empty());
    assert_eq!(cluster.len(), 0);
    assert!(cluster.nodes().is_empty());
}

#[tokio::test]
async fn cluster_reports_size_and_publish_routes_to_the_addressed_node() {
    let a = Arc::new(Mock::new(BackendKind::Mesh));
    let b = Arc::new(Mock::new(BackendKind::Mesh));
    let cluster = Cluster::new(vec![a.clone(), b.clone()]);

    assert_eq!(cluster.len(), 2);
    assert!(!cluster.is_empty());
    assert_eq!(cluster.nodes().len(), 2);
    assert_eq!(cluster.node(1).kind(), BackendKind::Mesh);

    cluster
        .publish_from(1, vec![7, 8, 9])
        .await
        .expect("publish");

    assert!(
        a.published().is_empty(),
        "publish_from(1, ..) must not reach node 0"
    );
    assert_eq!(
        b.published(),
        vec![vec![7, 8, 9]],
        "publish_from(1, ..) must hand the bytes to node 1"
    );
}

#[tokio::test]
async fn await_convergence_succeeds_when_every_node_already_holds_a_superset() {
    let expected: BTreeSet<EventId> = [eid(1), eid(2)].into_iter().collect();

    // Node 0 holds the expected set PLUS an extra id: convergence is subset,
    // not equality, so an over-full node still counts as converged.
    let mut superset = expected.clone();
    superset.insert(eid(9));
    let nodes = vec![
        Arc::new(Mock::with_ids(BackendKind::Mesh, superset)),
        Arc::new(Mock::with_ids(BackendKind::Mesh, expected.clone())),
    ];
    let cluster = Cluster::new(nodes);

    let started = Instant::now();
    let sets = cluster
        .await_convergence(&expected, Duration::from_secs(5))
        .await;

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an already-converged cluster must return promptly, not wait the deadline"
    );
    assert_eq!(sets.len(), 2);
    for held in &sets {
        assert!(expected.is_subset(held));
    }
}

#[tokio::test]
async fn await_convergence_returns_the_exact_partial_sets_after_the_deadline() {
    let expected: BTreeSet<EventId> = [eid(1), eid(2), eid(3)].into_iter().collect();
    let partial: BTreeSet<EventId> = [eid(1)].into_iter().collect();

    let nodes = vec![
        Arc::new(Mock::with_ids(BackendKind::Gossip, partial.clone())),
        Arc::new(Mock::with_ids(BackendKind::Gossip, BTreeSet::new())),
    ];
    let cluster = Cluster::new(nodes);

    let deadline = Duration::from_millis(80);
    let started = Instant::now();
    let sets = cluster.await_convergence(&expected, deadline).await;

    assert!(
        started.elapsed() >= deadline,
        "a cluster that never converges must wait out the whole deadline"
    );
    // The returned sets are the real per-node state, so the caller can diff
    // them against `expected` for the failure-mode ledger (spec §7.6).
    assert_eq!(sets[0], partial);
    assert!(sets[1].is_empty());
    assert!(!expected.is_subset(&sets[0]));
}

#[tokio::test]
async fn await_convergence_detects_ids_arriving_mid_flight() {
    let expected: BTreeSet<EventId> = [eid(1), eid(2)].into_iter().collect();

    // Nothing is present at first; a concurrent task fills the set well after
    // the first poll but far inside the deadline. The loop must detect the
    // arrival and return early — proving it actually polls, not just checks
    // once.
    let node = Arc::new(Mock::new(BackendKind::Mesh));
    let filler = node.clone();
    let to_add = expected.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(40)).await;
        filler.insert_all(&to_add);
    });

    let cluster = Cluster::new(vec![node]);
    let started = Instant::now();
    let sets = cluster
        .await_convergence(&expected, Duration::from_secs(5))
        .await;

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "convergence must be detected when ids arrive, not deferred to the deadline"
    );
    assert!(expected.is_subset(&sets[0]));
}
