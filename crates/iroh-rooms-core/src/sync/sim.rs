//! [`SimNet`] — the deterministic in-memory multi-peer simulation harness that
//! proves Gate D (spec `bounded-recent-sync-prototype.md` §8 / scope item 3).
//!
//! `SimNet` routes the engines' [`Outgoing`] frames between N [`SyncEngine`]s with
//! knobs for shuffled / delayed / dropped delivery, partitions, and
//! disconnect/reconnect. There is **no network, no async, and no wall clock** — an
//! injected `now_ms` and a seeded PRNG are the only sources of "time" and
//! "randomness", so every scenario is exactly reproducible (spec §8 / R4). This is
//! what makes "event set equality can be asserted after sync" a precise,
//! repeatable assertion ([`SimNet::assert_converged`]).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::event::ids::RoomId;

use super::config::SyncConfig;
use super::engine::{Completeness, SyncEngine};
use super::message::{Outgoing, PeerId};

/// A frame in flight between two peers (the engine's [`Outgoing`] plus its sender,
/// which the receiver needs as `on_message`'s `from`).
#[derive(Clone, Debug)]
struct Envelope {
    from: PeerId,
    to: PeerId,
    out: Outgoing,
}

/// A tiny deterministic `SplitMix64` PRNG for seeded shuffles (no `Math.random`,
/// no wall clock — spec §8 / R4).
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Hard bounds so a non-converging scenario fails loudly instead of looping
/// forever (a test bug should surface, not hang).
const MAX_DRAIN_STEPS: usize = 1_000_000;
const MAX_ROUNDS: usize = 10_000;

/// A deterministic in-memory mesh of [`SyncEngine`]s for one room.
pub struct SimNet {
    room_id: RoomId,
    engines: BTreeMap<PeerId, SyncEngine>,
    /// Undirected connectivity: `links[a]` is the set of peers `a` can reach.
    links: BTreeMap<PeerId, BTreeSet<PeerId>>,
    queue: VecDeque<Envelope>,
    clock_ms: u64,
    /// Frames dropped at delivery for exceeding the wire frame cap — mirrors the
    /// net writer's oversized-frame drop (issue #113), so a sync message the
    /// real transport could never deliver also never delivers in Gate-D
    /// simulation. Exposed via [`dropped_oversized`](Self::dropped_oversized)
    /// for regression assertions.
    dropped_oversized: u64,
}

impl SimNet {
    /// A fresh empty mesh for `room_id`.
    #[must_use]
    pub fn new(room_id: RoomId) -> Self {
        Self {
            room_id,
            engines: BTreeMap::new(),
            links: BTreeMap::new(),
            queue: VecDeque::new(),
            clock_ms: 0,
            dropped_oversized: 0,
        }
    }

    /// Register an already-built engine under `peer`.
    pub fn add_peer(&mut self, peer: PeerId, engine: SyncEngine) {
        self.engines.insert(peer, engine);
        self.links.entry(peer).or_default();
    }

    /// Build and register a fresh engine over an in-memory store for `peer`.
    ///
    /// # Errors
    /// Propagates [`SyncError`](super::SyncError) from engine construction.
    pub fn add_fresh_peer(
        &mut self,
        peer: PeerId,
        config: SyncConfig,
    ) -> Result<(), super::SyncError> {
        let store = crate::store::EventStore::open_in_memory().map_err(super::SyncError::Store)?;
        let engine = SyncEngine::open(store, self.room_id, config)?;
        self.add_peer(peer, engine);
        Ok(())
    }

    /// The registered peers, in deterministic order.
    #[must_use]
    pub fn peers(&self) -> Vec<PeerId> {
        self.engines.keys().copied().collect()
    }

    /// Borrow a peer's engine.
    ///
    /// # Panics
    /// Panics if `peer` is not registered.
    #[must_use]
    pub fn engine(&self, peer: PeerId) -> &SyncEngine {
        self.engines.get(&peer).expect("unknown peer")
    }

    /// Mutably borrow a peer's engine (e.g. to seed a starting log or publish).
    ///
    /// # Panics
    /// Panics if `peer` is not registered.
    pub fn engine_mut(&mut self, peer: PeerId) -> &mut SyncEngine {
        self.engines.get_mut(&peer).expect("unknown peer")
    }

    // ------------------------------------------------------------------
    // Connectivity
    // ------------------------------------------------------------------

    /// Bring up a link between `a` and `b` and run both connect handshakes.
    pub fn connect(&mut self, a: PeerId, b: PeerId) {
        self.links.entry(a).or_default().insert(b);
        self.links.entry(b).or_default().insert(a);
        let outs_a = self.engine_mut(a).on_connect(b);
        self.enqueue(a, outs_a);
        let outs_b = self.engine_mut(b).on_connect(a);
        self.enqueue(b, outs_b);
    }

    /// Bring up a full mesh between every registered pair.
    pub fn connect_all(&mut self) {
        let peers = self.peers();
        for (i, a) in peers.iter().enumerate() {
            for b in &peers[i + 1..] {
                self.connect(*a, *b);
            }
        }
    }

    /// Tear down the link between `a` and `b` (in-flight frames between them are
    /// dropped at delivery). The orphan park is retained for retry on reconnect.
    pub fn disconnect(&mut self, a: PeerId, b: PeerId) {
        self.links.entry(a).or_default().remove(&b);
        self.links.entry(b).or_default().remove(&a);
        self.engine_mut(a).on_disconnect(b);
        self.engine_mut(b).on_disconnect(a);
    }

    /// Re-establish a link and re-run the connect handshake (spec §6.3 reconnect).
    pub fn reconnect(&mut self, a: PeerId, b: PeerId) {
        self.connect(a, b);
    }

    /// Model a **process restart** of `peer` (spec D9 / AC5): tear down its live
    /// links (a restart loses every connection), drop the engine's in-memory
    /// session state, and re-`open` a fresh engine over the **same** store.
    ///
    /// Because the store (its `events` table and the v2 sync-cache tables) is
    /// reused, this exercises the restore-from-tables + re-fold path — the
    /// persisted park, unconfirmed suspicion, backfill token buckets, and
    /// trust-decision audit come back, while the park's `WantEvents` retry is
    /// re-issued on the next [`reconnect`](Self::reconnect)/[`tick`](Self::tick).
    /// The caller reconnects afterwards to resume traffic.
    ///
    /// # Errors
    /// Propagates [`SyncError`](super::SyncError) from re-opening the engine.
    ///
    /// # Panics
    /// Panics if `peer` is not registered.
    pub fn restart(&mut self, peer: PeerId) -> Result<(), super::SyncError> {
        // A restart loses every live connection; drop this peer's links first so
        // the mesh reflects reality and the caller must reconnect to resume.
        let linked: Vec<PeerId> = self
            .links
            .get(&peer)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        for other in linked {
            self.disconnect(peer, other);
        }
        // Drop in-flight frames to/from the restarted peer (a real restart loses
        // its socket buffers); other peers' traffic is unaffected.
        self.queue.retain(|env| env.from != peer && env.to != peer);
        let engine = self.engines.remove(&peer).expect("unknown peer");
        let config = engine.config();
        let store = engine.into_store();
        let restarted = SyncEngine::open(store, self.room_id, config)?;
        self.engines.insert(peer, restarted);
        Ok(())
    }

    /// Partition the mesh into two groups, tearing down every cross-group link.
    pub fn partition(&mut self, group_a: &[PeerId], group_b: &[PeerId]) {
        for a in group_a {
            for b in group_b {
                self.disconnect(*a, *b);
            }
        }
    }

    fn connected(&self, a: PeerId, b: PeerId) -> bool {
        self.links.get(&a).is_some_and(|s| s.contains(&b))
    }

    // ------------------------------------------------------------------
    // Driving traffic
    // ------------------------------------------------------------------

    /// Publish a locally-authored frame at `peer` and enqueue its fan-out.
    ///
    /// # Errors
    /// Propagates [`SyncError`](super::SyncError) if the frame is invalid.
    pub fn publish(&mut self, peer: PeerId, bytes: &[u8]) -> Result<(), super::SyncError> {
        let outs = self.engine_mut(peer).publish(bytes)?;
        self.enqueue(peer, outs);
        Ok(())
    }

    /// Inject a raw `WireEvent` frame into `peer` as if delivered from `from`, and
    /// enqueue any resulting frames (used to seed missed events / out-of-order
    /// delivery).
    pub fn deliver_raw(&mut self, peer: PeerId, from: PeerId, bytes: &[u8]) {
        let outs = self.engine_mut(peer).ingest_frame(from, bytes);
        self.enqueue(peer, outs);
    }

    /// Tick every engine once (advancing the injected clock), enqueuing outputs.
    pub fn tick(&mut self) {
        self.clock_ms += 1000;
        let now = self.clock_ms;
        for peer in self.peers() {
            let outs = self.engine_mut(peer).on_tick(now);
            self.enqueue(peer, outs);
        }
    }

    /// Deliver one queued frame (if its link is still up), enqueuing the
    /// receiver's response. Returns `false` when the queue is empty.
    ///
    /// A frame whose encoded body exceeds the wire cap is dropped here — at
    /// delivery, exactly where the net writer drops it — so the sim cannot
    /// deliver a message the real transport never could (issue #113). Checking
    /// at dequeue (not enqueue) keeps `shuffle` permutations and `restart`'s
    /// queue filtering byte-identical to the un-enforced harness.
    pub fn step(&mut self) -> bool {
        let Some(env) = self.queue.pop_front() else {
            return false;
        };
        if !self.connected(env.from, env.to) {
            // Link went down while in flight: drop (the engine re-pulls on
            // reconnect). Still counts as a step.
            return true;
        }
        if env.out.msg.encode().len() > super::message::MAX_FRAME_BYTES {
            self.dropped_oversized += 1;
            return true;
        }
        if self.engines.contains_key(&env.to) {
            let outs = self.engine_mut(env.to).on_message(env.from, env.out.msg);
            self.enqueue(env.to, outs);
        }
        true
    }

    /// Frames dropped at delivery for exceeding the wire frame cap (issue #113).
    #[must_use]
    pub fn dropped_oversized(&self) -> u64 {
        self.dropped_oversized
    }

    /// Deterministically permute the in-flight queue with a seeded PRNG
    /// (Fisher-Yates), modelling arbitrary arrival order (spec §8.1 / §10 vector).
    pub fn shuffle(&mut self, seed: u64) {
        let mut items: Vec<Envelope> = self.queue.drain(..).collect();
        let mut rng = SplitMix64(seed ^ 0xD1B5_4A32_D192_ED03);
        let n = items.len();
        for i in (1..n).rev() {
            let modulus = u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1);
            let j = usize::try_from(rng.next_u64() % modulus).unwrap_or(0);
            items.swap(i, j);
        }
        self.queue = items.into_iter().collect();
    }

    /// Drain the queue, then tick (anti-entropy re-pull + park retry) until a full
    /// tick+drain round makes no further progress — the mesh is quiescent.
    /// Bounded so a non-converging scenario fails loudly rather than hanging.
    ///
    /// A tick is always run at least once even when nothing is parked: a shuffled
    /// handshake can drop a chat frame that only a re-pull recovers (engine
    /// `on_tick` docs), so quiescence is "no new accepts and an empty queue",
    /// not merely "park empty".
    ///
    /// # Panics
    /// Panics if the mesh does not quiesce within the internal bound (a test bug
    /// or a real non-termination — either way it must surface).
    pub fn run_to_quiescence(&mut self) {
        for _ in 0..MAX_ROUNDS {
            self.drain();
            let before = self.progress_fingerprint();
            self.tick();
            self.drain();
            if self.progress_fingerprint() == before {
                return;
            }
        }
        panic!("SimNet did not quiesce within {MAX_ROUNDS} rounds");
    }

    fn drain(&mut self) {
        let mut steps = 0;
        while self.step() {
            steps += 1;
            assert!(steps <= MAX_DRAIN_STEPS, "SimNet drain exceeded step bound");
        }
    }

    fn total_parked(&self) -> usize {
        self.engines.values().map(SyncEngine::parked_len).sum()
    }

    /// A cheap progress signature: total accepted events + total parked frames.
    fn progress_fingerprint(&self) -> (u64, usize) {
        let accepted: u64 = self.engines.values().map(|e| e.counters().accepted).sum();
        (accepted, self.total_parked())
    }

    fn enqueue(&mut self, from: PeerId, outs: Vec<Outgoing>) {
        for out in outs {
            self.queue.push_back(Envelope {
                from,
                to: out.peer,
                out,
            });
        }
    }

    // ------------------------------------------------------------------
    // Convergence oracle (spec D8 / §8.1 AC4)
    // ------------------------------------------------------------------

    /// Assert every peer in `peers` holds the **identical full validated set**,
    /// admin tip, and membership snapshot. Use when no chat windowing is in play
    /// (the whole log fits the window), so full set equality must hold.
    ///
    /// # Panics
    /// Panics with a diff-style message if any peer diverges.
    pub fn assert_converged(&self, peers: &[PeerId]) {
        let Some((first, rest)) = peers.split_first() else {
            return;
        };
        let base = self.engine(*first).digest().expect("digest");
        for peer in rest {
            let other = self.engine(*peer).digest().expect("digest");
            assert_eq!(
                base.event_ids, other.event_ids,
                "event-set divergence between {first} and {peer}"
            );
            assert_eq!(
                base.admin_tip, other.admin_tip,
                "admin-tip divergence between {first} and {peer}"
            );
            assert_eq!(
                base.snapshot, other.snapshot,
                "snapshot divergence between {first} and {peer}"
            );
        }
    }

    /// Assert every peer in `peers` holds the **identical never-windowed
    /// authorization-class set, admin tip, and snapshot** — the unconditional
    /// guarantee that holds even when chat is bounded to different windows (spec
    /// §0 / AC2).
    ///
    /// # Panics
    /// Panics with a diff-style message if any peer's membership view diverges.
    pub fn assert_membership_converged(&self, peers: &[PeerId]) {
        let Some((first, rest)) = peers.split_first() else {
            return;
        };
        let base_ids = self
            .engine(*first)
            .membership_event_ids()
            .expect("membership ids");
        let base = self.engine(*first).digest().expect("digest");
        for peer in rest {
            let other_ids = self
                .engine(*peer)
                .membership_event_ids()
                .expect("membership ids");
            let other = self.engine(*peer).digest().expect("digest");
            assert_eq!(
                base_ids, other_ids,
                "membership sub-DAG divergence between {first} and {peer}"
            );
            assert_eq!(
                base.admin_tip, other.admin_tip,
                "admin-tip divergence between {first} and {peer}"
            );
            assert_eq!(
                base.snapshot, other.snapshot,
                "snapshot divergence between {first} and {peer}"
            );
        }
    }

    /// The completeness verdict at `peer` (security fail-closed assertions).
    #[must_use]
    pub fn completeness(&self, peer: PeerId) -> Completeness {
        self.engine(peer).completeness()
    }
}
