//! `HarnessCluster` — N in-process loopback nodes through the shipping
//! [`iroh_rooms_net::Node`], meshed into a full graph via `Node::connect_to`
//! (spec §4 D1 / D2, §6.4). This is the load-bearing measurement surface: the
//! same byte-bounded queues, dial-with-backoff loops, and #136 guarded state
//! transitions the shipping carrier runs in production, driven at over-cap
//! transport pressure without modifying any shipping code.
//!
//! ## Identity alignment (spec §6.4 step 1)
//!
//! Each node's iroh endpoint secret is derived from the **same** 32-byte seed
//! as its event-signing device key, so `endpoint.id() == device_id ==
//! EndpointId` byte-for-byte (the same identity-unification invariant the
//! shipping `iroh_rooms_net::demo::Participant` enforces). This means the
//! `AllowlistAdmission` constructed in this module admits exactly the right
//! devices and the audit sink records the real device ids.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::admission::AllowlistAdmission;
use iroh_rooms_net::{NetConfig, NetMode, Node, PeerConnState};

use crate::workload::{build_genesis_for_admin, deterministic_room_nonce};
use crate::RecordingAudit;

/// The anti-entropy tick used by every harness node (spec §6.4 step 3 — short
/// tick keeps anti-entropy / reconnect catch-up brisk; matches the shipping
/// loopback test posture).
pub const HARNESS_TICK: Duration = Duration::from_millis(150);

/// One harness node: the live [`Node`] handle plus the seeded identity
/// material needed to respawn it in the rebind scenario (spec §6.8 step 6).
///
/// The signing secrets are held as **seeds** (not as `SigningKey` directly)
/// because `iroh_rooms_core::event::keys::SigningKey` is not `Clone`, and the
/// rebind scenario must be able to re-derive the same identity/device keys
/// after a target shutdown/respawn.
pub struct HarnessNode {
    /// The node index (0 == admin / genesis author).
    pub index: usize,
    /// The node's identity key (`sender_id`).
    pub identity: IdentityKey,
    /// The node's 32-byte identity seed (re-derives the identity signing key).
    pub identity_seed: [u8; 32],
    /// The node's 32-byte device seed (also the iroh endpoint secret seed).
    pub device_seed: [u8; 32],
    /// The node's authenticated transport id (`EndpointId == device_id`).
    pub endpoint_id: EndpointId,
    /// The live node handle.
    pub node: Node,
}

impl HarnessNode {
    /// Re-derive this node's identity signing key from its seed.
    #[must_use]
    pub fn identity_secret(&self) -> SigningKey {
        SigningKey::from_seed(&self.identity_seed)
    }
    /// Re-derive this node's device signing key from its seed.
    #[must_use]
    pub fn device_secret(&self) -> SigningKey {
        SigningKey::from_seed(&self.device_seed)
    }
    /// A dialable loopback address for this node (out-of-band exchange).
    ///
    /// # Errors
    /// Propagates `Node::endpoint_addr`.
    pub fn endpoint_addr(&self) -> Result<EndpointAddr> {
        self.node.endpoint_addr()
    }

    /// Graceful shutdown.
    ///
    /// # Errors
    /// Propagates `Node::shutdown`.
    pub async fn shutdown(self) -> Result<()> {
        self.node.shutdown().await
    }
}

/// The full N-node loopback cluster: the shared recording audit, the room
/// id, the genesis id, the admin principal, and the per-node live handles.
pub struct HarnessCluster {
    /// The room every node is admitted to (transport scope; not a product
    /// supported active-member room — see D1 caveat).
    pub room_id: RoomId,
    /// The genesis event id (the chain root the workload parents on).
    pub genesis_id: iroh_rooms_core::event::EventId,
    /// The admin (node 0) principal — every load event is authored here.
    pub admin: AdminPrincipal,
    /// Per-node live handles, index-aligned.
    pub nodes: Vec<HarnessNode>,
    /// The shared recording audit sink (also installed on every node).
    pub audit: Arc<RecordingAudit>,
}

/// The admin (node 0) principal: the keys needed to author and sign load
/// events. Held at the cluster level because the workload publishes from here.
///
/// Like [`HarnessNode`], the signing secrets are held as **seeds** (not
/// `SigningKey`) because `SigningKey` is not `Clone` and the matrix runner
/// authors workloads across multiple rate windows off the same admin.
pub struct AdminPrincipal {
    /// The admin's identity key (`sender_id`).
    pub identity: IdentityKey,
    /// The admin's 32-byte identity seed.
    pub identity_seed: [u8; 32],
    /// The admin's 32-byte device seed (also the iroh endpoint secret seed).
    pub device_seed: [u8; 32],
    /// The admin's `EndpointId` (== `device_id`).
    pub endpoint_id: EndpointId,
}

impl AdminPrincipal {
    /// Re-derive the admin's identity signing key from its seed.
    #[must_use]
    pub fn identity_secret(&self) -> SigningKey {
        SigningKey::from_seed(&self.identity_seed)
    }
    /// Re-derive the admin's device signing key from its seed.
    #[must_use]
    pub fn device_secret(&self) -> SigningKey {
        SigningKey::from_seed(&self.device_seed)
    }
}

/// Deterministic seed material for one node, derived from a `u64` seed base.
#[derive(Clone)]
pub struct NodeSeeds {
    /// The node index.
    pub index: usize,
    /// 32-byte identity seed.
    pub identity_seed: [u8; 32],
    /// 32-byte device seed (also the iroh endpoint secret seed).
    pub device_seed: [u8; 32],
}

impl NodeSeeds {
    /// The identity-signing key.
    #[must_use]
    pub fn identity_secret(&self) -> SigningKey {
        SigningKey::from_seed(&self.identity_seed)
    }
    /// The device-signing key.
    #[must_use]
    pub fn device_secret(&self) -> SigningKey {
        SigningKey::from_seed(&self.device_seed)
    }
    /// The iroh endpoint secret (== the device signing key seed).
    #[must_use]
    pub fn iroh_secret(&self) -> SecretKey {
        SecretKey::from_bytes(&self.device_seed)
    }
    /// The transport id (`EndpointId == device_id`).
    #[must_use]
    pub fn endpoint_id(&self) -> EndpointId {
        self.iroh_secret().public()
    }
    /// The device key as the event layer sees it.
    #[must_use]
    pub fn device_key(&self) -> DeviceKey {
        self.device_secret().device_key()
    }
    /// The identity key.
    #[must_use]
    pub fn identity(&self) -> IdentityKey {
        self.identity_secret().identity_key()
    }
}

/// Build the deterministic seed material for N nodes given a `seed_base`.
/// Node `i`'s identity seed is `[seed_base + 2*i, ...]` and device seed is
/// `[seed_base + 2*i + 1, ...]`, so identity and device keys differ (Event
/// Protocol §1) and re-runs reproduce byte-identical principals.
#[must_use]
pub fn node_seeds(n: usize, seed_base: u64) -> Vec<NodeSeeds> {
    (0..n)
        .map(|i| {
            let identity_u64 = seed_base.wrapping_add(2 * i as u64);
            let device_u64 = seed_base.wrapping_add(2 * i as u64 + 1);
            NodeSeeds {
                index: i,
                identity_seed: seed_to_bytes(identity_u64),
                device_seed: seed_to_bytes(device_u64),
            }
        })
        .collect()
}

/// Spread a `u64` across a 32-byte seed (deterministic, mirrors the existing
/// spike-transport / `spike-nat` pattern).
fn seed_to_bytes(seed: u64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let s = seed.to_le_bytes();
    for chunk in bytes.chunks_mut(8) {
        let n = chunk.len().min(8);
        chunk[..n].copy_from_slice(&s[..n]);
    }
    bytes
}

/// Build a full mesh `AllowlistAdmission` that binds every node device to its
/// identity and marks each identity Active (spec §6.4 step 3 — the membership-
/// snapshot shape, D6, but transport-admit-only per D1 caveat).
#[must_use]
pub fn full_mesh_admission(seeds: &[NodeSeeds]) -> AllowlistAdmission {
    let mut auth = AllowlistAdmission::new();
    for s in seeds {
        auth = auth
            .bind_device(s.endpoint_id(), s.identity())
            .set_active(s.identity());
    }
    auth
}

impl HarnessCluster {
    /// Spawn N in-process loopback nodes, publish the genesis from node 0,
    /// wait for the full mesh to come up via every ordered `(i, j)` pair's
    /// `connect_to`, and await readiness up to `readiness_timeout`
    /// (spec §6.4 step 1-5 / D2).
    ///
    /// # Errors
    /// Returns an error if any endpoint fails to bind or genesis publish fails.
    pub async fn spawn(n: usize, seed_base: u64, readiness_timeout: Duration) -> Result<Self> {
        let audit = Arc::new(RecordingAudit::new());
        let seeds = node_seeds(n, seed_base);
        let admission = Arc::new(full_mesh_admission(&seeds));

        // Genesis: admin (node 0) authors room.created. Vary the nonce per
        // spawn so two scenarios in one process never collide on room_id.
        let admin_seeds = &seeds[0];
        let admin_identity_secret = admin_seeds.identity_secret();
        let admin_device_secret = admin_seeds.device_secret();
        let room_name = format!("spike-N40 n={n}");
        let nonce = deterministic_room_nonce(seed_base);
        let created_at = 1_770_000_000_000 + seed_base;
        let (room_id, genesis_wire) = build_genesis_for_admin(
            &admin_identity_secret,
            &admin_device_secret,
            &room_name,
            &nonce,
            created_at,
        );
        let genesis_id = iroh_rooms_core::event::signed::event_id_from_bytes(&genesis_wire.signed);

        let admin_principal = AdminPrincipal {
            identity: admin_identity_secret.identity_key(),
            identity_seed: admin_seeds.identity_seed,
            device_seed: admin_seeds.device_seed,
            endpoint_id: admin_seeds.endpoint_id(),
        };

        // Spawn every node first so all endpoint addresses are known.
        let mut nodes: Vec<HarnessNode> = Vec::with_capacity(n);
        for s in &seeds {
            let store = EventStore::open_in_memory().context("open in-memory event store")?;
            let engine = SyncEngine::open(store, room_id, SyncConfig::default())
                .context("open sync engine")?;
            let cfg = NetConfig {
                mode: NetMode::Loopback,
                ..NetConfig::default()
            };
            let node = Node::spawn(
                s.iroh_secret(),
                admission.clone(),
                audit.clone(),
                engine,
                cfg,
                HARNESS_TICK,
            )
            .await
            .with_context(|| format!("spawn node {}", s.index))?;
            nodes.push(HarnessNode {
                index: s.index,
                identity: s.identity(),
                identity_seed: s.identity_seed,
                device_seed: s.device_seed,
                endpoint_id: s.endpoint_id(),
                node,
            });
        }

        // Genesis: publish from node 0 (the admin) so every node's store
        // receives it through the mesh. Each node's local SyncEngine ingests
        // it on receipt.
        nodes[0]
            .node
            .publish(genesis_wire.to_bytes())
            .await
            .context("publish genesis from admin")?;

        // Full mesh: every ordered pair (i, j), i != j. This mirrors the
        // dial pressure of a managed v1 mesh where every active device dials
        // every other (D2).
        let addrs: Vec<EndpointAddr> = nodes
            .iter()
            .map(HarnessNode::endpoint_addr)
            .collect::<Result<Vec<_>>>()?;
        for (i, node) in nodes.iter().enumerate() {
            for (j, addr) in addrs.iter().enumerate() {
                if i == j {
                    continue;
                }
                node.node.connect_to(addr.clone());
            }
        }

        // Await readiness: every node reports `N - 1` Connected peers, or
        // the readiness timeout elapses (recorded as partial — the harness
        // still runs but the metrics reflect the partial state).
        Self::await_readiness(&nodes, readiness_timeout).await;

        Ok(Self {
            room_id,
            genesis_id,
            admin: admin_principal,
            nodes,
            audit,
        })
    }

    /// Wait until every node reports `expected` Connected peers, or
    /// `deadline` elapses (whichever comes first). Non-fatal: returns the
    /// final per-node connected counts; the caller records partial readiness
    /// in the metrics notes when not all nodes reached full mesh.
    async fn await_readiness(nodes: &[HarnessNode], deadline: Duration) -> Vec<usize> {
        let expected = nodes.len().saturating_sub(1);
        let start = Instant::now();
        let poll = Duration::from_millis(50);
        loop {
            let connected_counts: Vec<usize> = nodes
                .iter()
                .map(|hn| {
                    hn.node
                        .peer_states()
                        .iter()
                        .filter(|(_, s)| *s == PeerConnState::Connected)
                        .count()
                })
                .collect();
            let ready = connected_counts.iter().all(|&c| c >= expected);
            if ready || start.elapsed() >= deadline {
                return connected_counts;
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// The expected connected peer entries for this cluster: `N * (N - 1)`
    /// (every node's view of every other, both directions counted).
    #[must_use]
    pub fn expected_connected_peer_entries(&self) -> usize {
        expected_connected_peer_entries_for_n(self.nodes.len())
    }

    /// Per-node connected peer counts at this instant.
    #[must_use]
    pub fn connected_counts(&self) -> Vec<usize> {
        self.nodes
            .iter()
            .map(|hn| {
                hn.node
                    .peer_states()
                    .iter()
                    .filter(|(_, s)| *s == PeerConnState::Connected)
                    .count()
            })
            .collect()
    }

    /// Shut down every node, draining transports and pumps so a fresh
    /// scenario in the same process is not contaminated by leftover
    /// backlog/links (spec §10 reliability).
    ///
    /// # Errors
    /// Returns the first shutdown error encountered.
    pub async fn shutdown(self) -> Result<()> {
        let mut err: Option<anyhow::Error> = None;
        for hn in self.nodes {
            if let Err(e) = hn.shutdown().await {
                if err.is_none() {
                    err = Some(e);
                }
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Expected directed peer entries for an N-node full mesh.
#[must_use]
pub fn expected_connected_peer_entries_for_n(n: usize) -> usize {
    n.saturating_sub(1).saturating_mul(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_seeds_are_deterministic_and_distinct_per_index() {
        let a = node_seeds(3, 0x1000);
        let b = node_seeds(3, 0x1000);
        for (sa, sb) in a.iter().zip(b.iter()) {
            assert_eq!(sa.identity_seed, sb.identity_seed);
            assert_eq!(sa.device_seed, sb.device_seed);
            assert_eq!(sa.endpoint_id(), sb.endpoint_id());
        }
        // Distinct across indices.
        assert_ne!(a[0].endpoint_id(), a[1].endpoint_id());
        assert_ne!(a[1].endpoint_id(), a[2].endpoint_id());
        // Identity != device (Event Protocol §1).
        assert_ne!(a[0].identity_seed, a[0].device_seed);
    }

    #[test]
    fn endpoint_id_equals_device_id_byte_for_byte() {
        // The identity-unification invariant the harness relies on (Membership
        // §1 / spec A2). If this ever drifts the AllowlistAdmission would
        // silently bind the wrong id.
        let s = &node_seeds(1, 0x2000)[0];
        assert_eq!(s.endpoint_id().as_bytes(), s.device_key().as_bytes());
    }

    #[test]
    fn full_mesh_admission_admits_every_seed() {
        use iroh_rooms_net::Admission;
        use iroh_rooms_net::AdmissionDecision;
        let seeds = node_seeds(4, 0x3000);
        let auth = full_mesh_admission(&seeds);
        for s in &seeds {
            assert!(matches!(
                auth.authorize(s.endpoint_id()),
                AdmissionDecision::Admit { .. }
            ));
        }
    }

    #[test]
    fn full_mesh_admission_rejects_unseeded_stranger() {
        use iroh_rooms_net::Admission;
        use iroh_rooms_net::AdmissionDecision;
        use iroh_rooms_net::RejectCause;
        let seeds = node_seeds(2, 0x4000);
        let auth = full_mesh_admission(&seeds);
        let stranger = SecretKey::from_bytes(&[0xEE; 32]).public();
        assert_eq!(
            auth.authorize(stranger),
            AdmissionDecision::Reject(RejectCause::UnknownDevice)
        );
    }

    #[test]
    fn expected_connected_peer_entries_is_n_squared_minus_n() {
        assert_eq!(expected_connected_peer_entries_for_n(0), 0);
        assert_eq!(expected_connected_peer_entries_for_n(1), 0);
        assert_eq!(expected_connected_peer_entries_for_n(5), 20);
        assert_eq!(expected_connected_peer_entries_for_n(10), 90);
        assert_eq!(expected_connected_peer_entries_for_n(20), 380);
        assert_eq!(expected_connected_peer_entries_for_n(40), 1560);
    }
}
