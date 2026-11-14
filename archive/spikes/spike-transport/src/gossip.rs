//! The minimal `iroh-gossip` backend (spec §7.3).
//!
//! - One [`iroh_gossip::net::Gossip`] instance per node, registered on the
//!   shared `Router` on [`iroh_gossip::net::GOSSIP_ALPN`].
//! - `gossip.subscribe(TopicId, bootstrap_peers)` → a topic handle split into
//!   a sender and a receiver.
//! - `publish` = broadcast; the receiver task consumes `Event::Received`
//!   (dedup by recomputed `event_id`) and records `Event::Lagged` as a
//!   [`crate::BackendEvent::Lagged`] failure signal.
//! - **No admission:** any node that knows the 32-byte `TopicId` can join the
//!   topic and both receive and publish plaintext — the admission scenario
//!   (§7.6) stands up an *interloper* that knows only the topic and confirms
//!   this.
//! - **No history:** the late-join scenario subscribes the newcomer *after* M
//!   broadcasts and counts received pre-join events (expected 0).

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use iroh_rooms_core::event::signed::event_id_from_bytes;
use iroh_rooms_core::event::{EventId, WireEvent};

use crate::{BackendEvent, BackendKind, TransportBackend, WireBytes};

/// The fixed topic id the workload's events are broadcast on (all nodes in
/// one cluster join the same topic; a byte-fixed id keeps the harness
/// deterministic).
#[must_use]
pub fn event_topic() -> TopicId {
    TopicId::from_bytes([0x45; 32])
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    received: Mutex<BTreeSet<EventId>>,
    events: Mutex<Vec<BackendEvent>>,
}

impl Inner {
    fn record_frame(&self, bytes: &[u8]) {
        if let Ok(wire) = WireEvent::decode(bytes) {
            let id = event_id_from_bytes(&wire.signed);
            lock(&self.received).insert(id);
        }
    }
}

/// The dedicated liveness topic for the Residual-13 admin-tip-carrier probe
/// (spec §7.7) — distinct from [`event_topic`] so admin-tip advertisement sits
/// off the critical path, exactly the sanctioned optional use ADR-1 parks
/// gossip for.
#[must_use]
pub fn liveness_topic() -> TopicId {
    TopicId::from_bytes([0x4C; 32]) // 'L'iveness topic
}

/// A minimal gossip node subscribed to [`event_topic`].
pub struct GossipNode {
    inner: Arc<Inner>,
    endpoint: Endpoint,
    router: Router,
    gossip: Gossip,
    sender: GossipSender,
}

impl GossipNode {
    /// Bind a loopback gossip node and subscribe it to the shared event
    /// topic, bootstrapping from `bootstrap` (empty for the first node in a
    /// cluster).
    ///
    /// # Errors
    /// Returns an error if the endpoint fails to bind or the topic
    /// subscription fails.
    pub async fn spawn(secret: SecretKey, bootstrap: Vec<EndpointAddr>) -> Result<Self> {
        // Loopback has no real address-lookup/discovery service: a
        // `MemoryLookup` is iroh's manual "out-of-band addressing info" seam
        // (its own doc example is exactly this — pre-registering a peer's
        // address before dialing), the gossip-subscribe equivalent of how the
        // mesh backend and `spike-nat` dial by explicit `EndpointAddr` on
        // loopback.
        let address_lookup = MemoryLookup::new();
        for addr in &bootstrap {
            address_lookup.add_endpoint_info(addr.clone());
        }
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Disabled)
            .address_lookup(address_lookup)
            .bind()
            .await
            .context("bind loopback gossip endpoint")?;

        let bootstrap_ids: Vec<EndpointId> = bootstrap.iter().map(|addr| addr.id).collect();

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let router = Router::builder(endpoint.clone())
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();

        let has_bootstrap = !bootstrap_ids.is_empty();
        let topic = gossip
            .subscribe(event_topic(), bootstrap_ids)
            .await
            .context("subscribe to event topic")?;
        let (sender, mut receiver) = topic.split();

        // A node with a bootstrap peer waits until the swarm-membership layer
        // has actually formed at least one direct neighbor link before this
        // constructor returns — otherwise an immediate `publish()` can race
        // ahead of PlumTree/HyParView still settling the topology (the first
        // node in a cluster has no peer to join yet, so it skips this).
        if has_bootstrap {
            tokio::time::timeout(Duration::from_secs(5), receiver.joined())
                .await
                .context("timed out waiting to join the event topic swarm")?
                .context("join the event topic swarm")?;
        }

        let inner = Arc::new(Inner {
            received: Mutex::new(BTreeSet::new()),
            events: Mutex::new(Vec::new()),
        });
        tokio::spawn(receiver_task(inner.clone(), receiver));

        Ok(Self {
            inner,
            endpoint,
            router,
            gossip,
            sender,
        })
    }

    /// A clone of the underlying `Gossip` handle, so the admin-tip probe can
    /// subscribe this node to the *separate* [`liveness_topic`] without
    /// standing up a second endpoint (spec §7.7).
    #[must_use]
    pub fn gossip(&self) -> Gossip {
        self.gossip.clone()
    }

    /// This node's authenticated identity.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// A loopback dial address for this node (a bootstrap hint for a later
    /// joiner).
    ///
    /// # Errors
    /// Returns an error if the endpoint has no bound UDP socket.
    pub fn addr(&self) -> Result<EndpointAddr> {
        loopback_addr(&self.endpoint)
    }

    /// Gracefully stop this node.
    ///
    /// # Errors
    /// Returns an error if the router shutdown fails to join.
    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await.context("router shutdown")?;
        Ok(())
    }
}

async fn receiver_task(inner: Arc<Inner>, mut receiver: GossipReceiver) {
    loop {
        match receiver.next().await {
            Some(Ok(Event::Received(msg))) => inner.record_frame(&msg.content),
            Some(Ok(Event::Lagged)) => lock(&inner.events).push(BackendEvent::Lagged),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break,
        }
    }
}

#[async_trait]
impl TransportBackend for GossipNode {
    fn kind(&self) -> BackendKind {
        BackendKind::Gossip
    }

    async fn publish(&self, wire: WireBytes) -> anyhow::Result<()> {
        // A node holds what it authors (gossip does not loop a broadcast back
        // to its own sender), not only what a peer relays to it.
        self.inner.record_frame(&wire);
        self.sender.broadcast(Bytes::from(wire)).await?;
        Ok(())
    }

    fn received_ids(&self) -> BTreeSet<EventId> {
        lock(&self.inner.received).clone()
    }

    fn drain_events(&self) -> Vec<BackendEvent> {
        std::mem::take(&mut lock(&self.inner.events))
    }
}

/// Bring up `n` gossip nodes on loopback, all subscribed to [`event_topic`]:
/// node 0 has no bootstrap; every later node bootstraps from node 0's address
/// (a single shared rendezvous point — sufficient at N≤5, spec §2 non-goals:
/// no large-swarm partial-view membership evaluation).
///
/// # Errors
/// Returns an error if any bind or subscribe fails.
pub async fn spawn_swarm(n: usize, seed_base: u64) -> Result<Vec<Arc<GossipNode>>> {
    let mut nodes = Vec::with_capacity(n);
    let first = Arc::new(GossipNode::spawn(secret_from_seed(seed_base), Vec::new()).await?);
    let first_addr = first.addr()?;
    nodes.push(first);
    for i in 1..n {
        let node = GossipNode::spawn(
            secret_from_seed(seed_base + i as u64),
            vec![first_addr.clone()],
        )
        .await?;
        nodes.push(Arc::new(node));
    }
    Ok(nodes)
}

/// Deterministically derive a 32-byte secret from a `u64` seed (mirrors
/// `crate::mesh::secret_from_seed`, kept separate so the two backends never
/// accidentally share identities in a mixed scenario).
#[must_use]
pub fn secret_from_seed(seed: u64) -> SecretKey {
    let mut bytes = [0u8; 32];
    let s = seed.to_le_bytes();
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&s);
    }
    SecretKey::from_bytes(&bytes)
}

/// The gossip side of the AC3 admission probe (spec §7.6/§8): a node that
/// knows only the topic id and one bootstrap address subscribes and confirms
/// it is admitted with **no** authentication check — the "open topic"
/// evidence. Returns the interloper's node so the caller can confirm it can
/// also publish and be received.
///
/// # Errors
/// Returns an error if the endpoint fails to bind or the subscribe itself
/// fails (which would actually be the *unexpected* outcome for gossip).
pub async fn interloper_join(bootstrap: EndpointAddr, seed: u64) -> Result<GossipNode> {
    GossipNode::spawn(secret_from_seed(seed), vec![bootstrap]).await
}

/// Subscribe `gossip` to the [`liveness_topic`], bootstrapping from
/// `bootstrap` (empty for the first node). The gossip side of the Residual-13
/// admin-tip-carrier probe (spec §7.7): the same `AdminTip` `SyncMessage` the
/// mesh backend sends as a control frame (`mesh::MeshNode::send_control`) is
/// instead broadcast on this dedicated, off-critical-path topic.
///
/// Mirrors [`GossipNode::spawn`]'s join-wait for the event topic: a
/// subscriber with a bootstrap peer waits until the swarm-membership layer
/// has actually formed a direct neighbor link before returning, so a caller
/// that immediately broadcasts never races ahead of topology formation.
///
/// # Errors
/// Returns an error if the subscribe fails, or if a bootstrapped subscriber
/// times out waiting to join the topic swarm.
pub async fn subscribe_liveness(
    gossip: &Gossip,
    bootstrap: Vec<EndpointId>,
) -> Result<(GossipSender, GossipReceiver)> {
    let has_bootstrap = !bootstrap.is_empty();
    let topic = gossip
        .subscribe(liveness_topic(), bootstrap)
        .await
        .context("subscribe to liveness topic")?;
    let (sender, mut receiver) = topic.split();
    if has_bootstrap {
        tokio::time::timeout(Duration::from_secs(5), receiver.joined())
            .await
            .context("timed out waiting to join the liveness topic swarm")?
            .context("join the liveness topic swarm")?;
    }
    Ok((sender, receiver))
}

/// Lines of implementation code in this file (excludes the `#[cfg(test)]`
/// module) — the complexity-dimension proxy (spec §6).
#[must_use]
pub fn loc() -> usize {
    let src = include_str!("gossip.rs");
    src.split("#[cfg(test)]")
        .next()
        .unwrap_or(src)
        .lines()
        .count()
}

/// Build a loopback [`EndpointAddr`] (`id + 127.0.0.1:<bound port>`), bypassing
/// relay/DNS discovery.
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

#[cfg(test)]
mod tests {
    use super::{event_topic, secret_from_seed};

    #[test]
    fn event_topic_is_fixed_and_deterministic() {
        assert_eq!(event_topic(), event_topic());
    }

    #[test]
    fn secret_from_seed_is_deterministic_and_seed_sensitive() {
        assert_eq!(secret_from_seed(7).public(), secret_from_seed(7).public());
        assert_ne!(secret_from_seed(1).public(), secret_from_seed(2).public());
    }

    #[test]
    fn liveness_topic_is_distinct_from_the_event_topic() {
        // The admin-tip carrier (Residual 13) must sit on its own topic, off
        // the critical event path — a collision would fold liveness hints into
        // the load-bearing event set.
        assert_ne!(super::liveness_topic(), event_topic());
    }

    #[test]
    fn loc_is_a_positive_count_bounded_by_the_file_length() {
        // `loc()` is the complexity-dimension input feeding
        // `ComparisonResult.backend_loc`; it must report a positive line count
        // no larger than the whole file (it trims at the first `#[cfg(test)]`),
        // never zero or a panic.
        let total = include_str!("gossip.rs").lines().count();
        let impl_loc = super::loc();
        assert!(impl_loc > 0, "complexity proxy must be a positive count");
        assert!(
            impl_loc <= total,
            "loc() ({impl_loc}) cannot exceed the file's line count ({total})"
        );
    }
}
