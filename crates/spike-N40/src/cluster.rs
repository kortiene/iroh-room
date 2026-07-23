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

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::binding::DeviceBinding;
use iroh_rooms_core::event::capability_hash;
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey, SigningKey};
use iroh_rooms_core::event::{build_member_invited, build_member_joined, validate_wire_bytes};
use iroh_rooms_core::membership::RoomMembership;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::admission::{AdmissionView, AllowlistAdmission, SnapshotAdmission};
use iroh_rooms_net::{NetConfig, NetMode, Node, PeerConnState, GOSSIP_BOOTSTRAP_SEEDS};

use crate::workload::{build_genesis_for_admin, deterministic_room_nonce};
use crate::RecordingAudit;

/// The anti-entropy tick used by every harness node (spec §6.4 step 3 — short
/// tick keeps anti-entropy / reconnect catch-up brisk; matches the shipping
/// loopback test posture).
pub const HARNESS_TICK: Duration = Duration::from_millis(150);

/// How the harness forms live transport links after spawning nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectMode {
    /// Explicitly call `Node::connect_to` for every ordered pair.
    FullMesh,
    /// Let the managed-room `PeerManager` and gossip overlay form the bounded
    /// seed topology from the live membership snapshot.
    Gossip,
}

impl ConnectMode {
    /// Stable CLI/JSON label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FullMesh => "full-mesh",
            Self::Gossip => "gossip",
        }
    }

    /// Parse the `--connect-mode` value.
    ///
    /// # Errors
    ///
    /// Returns an error for any value other than `full-mesh` or `gossip`.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "full-mesh" => Ok(Self::FullMesh),
            "gossip" => Ok(Self::Gossip),
            other => {
                anyhow::bail!("invalid --connect-mode {other:?}; expected full-mesh or gossip")
            }
        }
    }
}

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
    /// How this cluster formed its transport links.
    pub connect_mode: ConnectMode,
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

struct MembershipFixture {
    room_id: RoomId,
    genesis_id: iroh_rooms_core::event::EventId,
    admin: AdminPrincipal,
    wires: Vec<Vec<u8>>,
    view: AdmissionView,
}

impl MembershipFixture {
    fn build(seeds: &[NodeSeeds], seed_base: u64) -> Result<Self> {
        let admin_seeds = &seeds[0];
        let admin_identity_secret = admin_seeds.identity_secret();
        let admin_device_secret = admin_seeds.device_secret();
        let room_name = format!("spike-N40 n={}", seeds.len());
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
        let admin = AdminPrincipal {
            identity: admin_identity_secret.identity_key(),
            identity_seed: admin_seeds.identity_seed,
            device_seed: admin_seeds.device_seed,
            endpoint_id: admin_seeds.endpoint_id(),
        };

        let mut wires = vec![genesis_wire.to_bytes()];
        let mut prev = genesis_id;
        for (idx, s) in seeds.iter().enumerate().skip(1) {
            let invite_id = short_id(seed_base, idx as u64, 0);
            let secret = short_id(seed_base, idx as u64, 1);
            let cap = capability_hash(&room_id, &invite_id, &secret);
            let invited = build_member_invited(
                &admin_identity_secret,
                &admin_device_secret,
                &room_id,
                &invite_id,
                &cap,
                "member",
                &s.identity(),
                None,
                None,
                &[prev],
                created_at + (idx as u64 * 2),
            );
            let invite_event_id =
                iroh_rooms_core::event::signed::event_id_from_bytes(&invited.signed);
            let member_identity = s.identity_secret();
            let member_device = s.device_secret();
            let binding =
                DeviceBinding::create(&room_id, &member_identity, member_device.device_key());
            let joined = build_member_joined(
                &member_identity,
                &member_device,
                &room_id,
                &invite_id,
                &secret,
                "member",
                binding,
                None,
                &[invite_event_id],
                created_at + (idx as u64 * 2) + 1,
            );
            prev = iroh_rooms_core::event::signed::event_id_from_bytes(&joined.signed);
            wires.push(invited.to_bytes());
            wires.push(joined.to_bytes());
        }

        let validated = wires
            .iter()
            .map(|wire| {
                validate_wire_bytes(
                    wire,
                    &iroh_rooms_core::event::ValidationContext::for_room(room_id),
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|reason| {
                anyhow::anyhow!("fixture membership event failed validation: {reason:?}")
            })?;
        let membership = RoomMembership::from_events(room_id, validated);
        for member in membership.snapshot().active_members() {
            if member.device.is_some() {
                continue;
            }
            anyhow::bail!("active fixture member has no bound device");
        }
        let active = membership.snapshot().active_member_count();
        if active != seeds.len() {
            anyhow::bail!(
                "fixture membership active count {active} != node count {}",
                seeds.len()
            );
        }
        let view = AdmissionView::from_snapshot(&membership.snapshot(), &[]);

        Ok(Self {
            room_id,
            genesis_id,
            admin,
            wires,
            view,
        })
    }
}

fn short_id(seed_base: u64, index: u64, salt: u64) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&seed_base.wrapping_add(index).to_le_bytes());
    out[8..].copy_from_slice(&salt.to_le_bytes());
    out
}

fn planned_loopback_addr(seed: &NodeSeeds, seed_base: u64) -> EndpointAddr {
    let socket = SocketAddr::from((Ipv4Addr::LOCALHOST, loopback_port(seed_base, seed.index)));
    EndpointAddr::new(seed.endpoint_id()).with_ip_addr(socket)
}

fn loopback_port(seed_base: u64, index: usize) -> u16 {
    let base = 30_000 + u16::try_from(seed_base % 20_000).expect("port base fits u16");
    base.saturating_add(u16::try_from(index).expect("spike-N40 index fits u16"))
}

impl HarnessCluster {
    /// Spawn N in-process loopback nodes and form links according to
    /// `connect_mode` (spec §6.4 step 1-5 / D2).
    ///
    /// # Errors
    /// Returns an error if any endpoint fails to bind or fixture publishing fails.
    pub async fn spawn(
        n: usize,
        seed_base: u64,
        readiness_timeout: Duration,
        connect_mode: ConnectMode,
    ) -> Result<Self> {
        let audit = Arc::new(RecordingAudit::new());
        let seeds = node_seeds(n, seed_base);
        let fixture = MembershipFixture::build(&seeds, seed_base)?;
        let planned_addrs: Vec<EndpointAddr> = seeds
            .iter()
            .map(|seed| planned_loopback_addr(seed, seed_base))
            .collect();
        let admission = Arc::new(full_mesh_admission(&seeds));

        let mut nodes: Vec<HarnessNode> = Vec::with_capacity(n);
        for s in &seeds {
            let store = EventStore::open_in_memory().context("open in-memory event store")?;
            let mut engine = SyncEngine::open(store, fixture.room_id, SyncConfig::default())
                .context("open sync engine")?;
            for wire in &fixture.wires {
                engine.publish(wire).context("seed membership fixture")?;
            }
            let cfg = NetConfig {
                mode: NetMode::Loopback,
                loopback_bind_addr: matches!(connect_mode, ConnectMode::Gossip).then_some(
                    SocketAddr::from((Ipv4Addr::LOCALHOST, loopback_port(seed_base, s.index))),
                ),
                ..NetConfig::default()
            };
            let node = match connect_mode {
                ConnectMode::FullMesh => {
                    Node::spawn(
                        s.iroh_secret(),
                        admission.clone(),
                        audit.clone(),
                        engine,
                        cfg,
                        HARNESS_TICK,
                    )
                    .await
                }
                ConnectMode::Gossip => {
                    let cell = Arc::new(Mutex::new(fixture.view.clone()));
                    let snapshot_admission = Arc::new(SnapshotAdmission::new(cell.clone()));
                    let hints = planned_addrs
                        .iter()
                        .enumerate()
                        .filter(|(idx, _)| *idx != s.index)
                        .map(|(_, addr)| addr.clone())
                        .collect();
                    Node::spawn_room(
                        s.iroh_secret(),
                        snapshot_admission,
                        audit.clone(),
                        engine,
                        cfg,
                        HARNESS_TICK,
                        hints,
                        cell,
                        None,
                    )
                    .await
                }
            }
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

        match connect_mode {
            ConnectMode::FullMesh => {
                let addrs: Vec<EndpointAddr> = nodes
                    .iter()
                    .map(HarnessNode::endpoint_addr)
                    .collect::<Result<Vec<_>>>()?;
                for (i, node) in nodes.iter().enumerate() {
                    for (j, addr) in addrs.iter().enumerate() {
                        if i != j {
                            node.node.connect_to(addr.clone());
                        }
                    }
                }
            }
            ConnectMode::Gossip => {}
        }

        Self::await_readiness(&nodes, readiness_timeout, connect_mode).await;

        Ok(Self {
            connect_mode,
            room_id: fixture.room_id,
            genesis_id: fixture.genesis_id,
            admin: fixture.admin,
            nodes,
            audit,
        })
    }

    /// Wait until every node reports the mode-specific minimum readiness, or
    /// `deadline` elapses. Non-fatal: returns the final per-node connected counts.
    async fn await_readiness(
        nodes: &[HarnessNode],
        deadline: Duration,
        connect_mode: ConnectMode,
    ) -> Vec<usize> {
        let expected = expected_connected_peers_per_node(nodes.len(), connect_mode);
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
            let ready = match connect_mode {
                ConnectMode::FullMesh => connected_counts.iter().all(|&c| c >= expected),
                ConnectMode::Gossip => {
                    connected_counts.iter().sum::<usize>() >= expected * nodes.len()
                }
            };
            if ready || start.elapsed() >= deadline {
                return connected_counts;
            }
            tokio::time::sleep(poll).await;
        }
    }

    /// The expected connected peer entries for this cluster.
    #[must_use]
    pub fn expected_connected_peer_entries(&self) -> usize {
        expected_connected_peer_entries_for_n(self.nodes.len(), self.connect_mode)
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

/// Expected directed peer entries for an N-node cluster under `connect_mode`.
#[must_use]
pub fn expected_connected_peer_entries_for_n(n: usize, connect_mode: ConnectMode) -> usize {
    expected_connected_peers_per_node(n, connect_mode).saturating_mul(n)
}

fn expected_connected_peers_per_node(n: usize, connect_mode: ConnectMode) -> usize {
    match connect_mode {
        ConnectMode::FullMesh => n.saturating_sub(1),
        ConnectMode::Gossip => n.saturating_sub(1).min(GOSSIP_BOOTSTRAP_SEEDS),
    }
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
    fn expected_connected_peer_entries_tracks_connect_mode() {
        assert_eq!(
            expected_connected_peer_entries_for_n(0, ConnectMode::FullMesh),
            0
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(1, ConnectMode::FullMesh),
            0
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(5, ConnectMode::FullMesh),
            20
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(10, ConnectMode::FullMesh),
            90
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(20, ConnectMode::FullMesh),
            380
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(40, ConnectMode::FullMesh),
            1560
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(5, ConnectMode::Gossip),
            15
        );
        assert_eq!(
            expected_connected_peer_entries_for_n(40, ConnectMode::Gossip),
            120
        );
    }
}
