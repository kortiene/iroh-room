//! The deterministic, sans-IO bounded recent-sync engine (spec
//! `bounded-recent-sync-prototype.md` §5 D1/D4/D5/D6/D8, §6 protocol).
//!
//! [`SyncEngine`] is a pure state machine over the landed
//! [`EventStore`](crate::store::EventStore) and
//! [`RoomMembership`](crate::membership::RoomMembership) fold. Every entry point
//! (`ingest_frame` / `publish` / `on_connect` / `on_message` / `on_tick`) consumes
//! local state + one input and **returns** the frames to send ([`Outgoing`]); it
//! performs no networking, no async, and no wall-clock reads (an advisory `now_ms`
//! is injected only for the advisory chat-time window). This is what makes
//! shuffled / dropped / partitioned / reconnect scenarios deterministically
//! reproducible (Gate D).
//!
//! The engine **orchestrates, it does not re-decide** (spec D4): every inbound
//! frame runs the exact landed path —
//! [`validate_wire_bytes`](crate::event::validate_wire_bytes) →
//! [`RoomMembership::ingest`] — and only fold-`Accepted` events are persisted
//! (spec D5), so the `events` table stays equal to the convergent validated set
//! (an insert the store fails is retried on tick until it lands or its bounded
//! budget surfaces a CRITICAL `store_degraded` decision, issue #119).
//! The engine adds: backfill (pull), anti-amplification gating, fan-out, and the
//! admin-tip / fail-closed completeness layer.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::event::content::{Content, EventType};
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::IdentityKey;
use crate::event::reject::RejectReason;
use crate::event::signed;
use crate::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
use crate::event::wire::WireEvent;
use crate::membership::{MembershipSnapshot, RoomMembership, Status};
use crate::store::{
    EventStore, InsertOutcome, ParkedRow, StoreError, StoredEvent, SyncStateRow, TrustRow,
};

use super::config::SyncConfig;
use super::message::{id_set, Outgoing, PeerId, SyncMessage, Window};

/// An engine-level fault (never a single invalid event — those are logged drops,
/// spec §9). `Display` carries a stable lowercase code.
#[derive(Debug)]
#[non_exhaustive]
pub enum SyncError {
    /// An underlying event-store failure.
    Store(StoreError),
    /// A frame handed to [`SyncEngine::publish`] failed stateless validation.
    InvalidFrame(crate::event::RejectReason),
    /// The [`SyncConfig`] bounds were unusable.
    Config(&'static str),
    /// A frame handed to [`SyncEngine::publish`] is too large to ever deliver:
    /// even alone in an [`Events`](SyncMessage::Events) message it would exceed
    /// the [`MAX_FRAME_BYTES`](super::message::MAX_FRAME_BYTES) wire cap, so
    /// every peer's writer would drop it and the author would diverge silently
    /// (issue #113). Refusing at publish keeps undeliverable events off the log.
    /// This is a **local authoring** bound only — never a validation rule for
    /// remote frames, whose size the inbound framing layer already caps.
    OversizedFrame {
        /// The encoded `WireEvent` length.
        frame_len: usize,
    },
}

impl core::fmt::Display for SyncError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "sync_store_error: {e}"),
            Self::InvalidFrame(r) => write!(f, "sync_invalid_frame: {}", r.code()),
            Self::Config(c) => write!(f, "sync_config_error: {c}"),
            Self::OversizedFrame { frame_len } => {
                write!(f, "sync_oversized_frame: {frame_len}")
            }
        }
    }
}

impl std::error::Error for SyncError {}

impl From<StoreError> for SyncError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// The room's admin-completeness verdict — the load-bearing fail-closed predicate
/// the access planes consult (spec D6 / §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completeness {
    /// The local admin view is as complete as anything advertised in the room.
    Complete,
    /// A higher admin tip is known but not yet backfilled: the node *might* be
    /// missing a removal and **fails closed** on removal-sensitive decisions for
    /// the affected subjects until it catches up.
    AdminViewSuspect,
    /// Two distinct admin events were observed at the same `admin_seq` — the
    /// detectable signature of an admin self-fork (spec §7); fail closed on
    /// contested subjects and raise a CRITICAL `equivocation` alert.
    AdminForkDetected,
}

/// Severity of a [`TrustDecision`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Advisory: catch-up will resolve it.
    Warning,
    /// Non-recoverable safety event (admin equivocation, a degraded local
    /// store).
    Critical,
}

/// A first-class trust event surfaced for the audit surface (spec §9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustDecision {
    /// Stable code: `equivocation` (admin fork), `admin_view_suspect`,
    /// `store_degraded` (an accepted event could not be persisted, issue #119),
    /// `backfill_depth_exceeded` (a chain gap deeper than `max_backfill_depth`
    /// was dropped as a phantom parent — permanently unrecoverable, fix 2),
    /// `park_overflow` (a parked frame was cap-evicted and may be lost, fix 2),
    /// or `admin_tip_expired` (an unconfirmed admin tip expired, failing the
    /// removal-sensitive access gate OPEN, fix 2).
    pub code: &'static str,
    /// Severity (CRITICAL on an admin fork, a degraded store, a permanent event
    /// loss, or a fail-open tip expiry).
    pub severity: Severity,
    /// The `admin_seq` the decision concerns (the expired tip's seq for
    /// `admin_tip_expired`; `0` for decisions that concern no admin event, e.g.
    /// `store_degraded` / `backfill_depth_exceeded` / `park_overflow`).
    pub admin_seq: u64,
    /// The event ids involved (both branch tips for a fork; the unpersistable
    /// event for `store_degraded`; the lost event for `backfill_depth_exceeded`
    /// and `park_overflow`; the abandoned tip for `admin_tip_expired`).
    pub event_ids: Vec<EventId>,
}

/// The set-equality / convergence oracle (spec D8).
///
/// Convergence is asserted as: for the **never-windowed** sub-DAG and the
/// [`MembershipSnapshot`], two peers' digests are equal unconditionally; for
/// **chat**, equality holds within matched window parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncDigest {
    /// Every validated event id the peer holds.
    pub event_ids: BTreeSet<EventId>,
    /// The admin-chain tip.
    pub admin_tip: Option<(EventId, u64)>,
    /// The folded membership snapshot.
    pub snapshot: MembershipSnapshot,
}

/// Counters for the Gate-D evidence memo (spec §9). Deterministic given inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncCounters {
    /// Events accepted into the validated set (first time).
    pub accepted: u64,
    /// Idempotent duplicates ignored.
    pub duplicates: u64,
    /// Events rejected by the fold gate.
    pub rejected: u64,
    /// Frames parked as causally-incomplete orphans.
    pub parked: u64,
    /// Parked frames evicted by a cap.
    pub park_evicted: u64,
    /// `WantEvents` backfill messages emitted.
    pub backfill_requests: u64,
    /// Backfill requests suppressed by the token bucket.
    pub backfill_rate_limited: u64,
    /// Buffered frames dropped at the signer pre-check (non-member junk).
    pub signer_dropped: u64,
    /// Buffered frames dropped for exceeding the backfill depth bound.
    pub phantom_depth_dropped: u64,
    /// Frames fanned out / served to peers.
    pub frames_sent: u64,

    // -- restart-durability evidence (IR-0201 §9) -------------------------------
    /// Parked frames restored from `sync_parked` on `open`.
    pub parked_restored: u64,
    /// Restored parked rows dropped because their `wire` failed re-validation
    /// (corrupt/tampered on disk; spec D5).
    pub park_corrupt_dropped: u64,
    /// Set to 1 when an unconfirmed admin-tip suspicion was restored on `open`
    /// (anti fail-open; spec §1.1 / D3).
    pub suspicion_restored: u64,
    /// Trust-decision audit rows restored from `trust_decisions` on `open`.
    pub trust_restored: u64,
    /// Per-author backfill token buckets restored on `open` (the amplification
    /// budget is not reset by the restart; spec §1.3 / R4).
    pub tokens_restored: u64,

    // -- store-degradation evidence (issue #119) --------------------------------
    /// `store.insert` failures on a fold-accepted event (first attempts and
    /// retries alike). Non-zero means the fold and the store disagreed at least
    /// transiently this session.
    pub store_insert_failed: u64,
    /// Fold-accepted events abandoned by the insert-retry path (budget
    /// exhausted, or the retry queue was full on arrival). Each one also
    /// records a CRITICAL `store_degraded` [`TrustDecision`].
    pub store_retry_dropped: u64,

    // -- early event-id dedup evidence (issue #143) -----------------------------
    /// Event-id replays dropped by the in-memory cache **before** signature
    /// verification or any store work (issue #143 / #134 §22.2). Distinct from
    /// [`duplicates`](Self::duplicates), which counts duplicates discovered
    /// *after* the full validation + store path. A replay inside the cache
    /// window is ignored silently — its signed bytes are already durably
    /// stored — so the existing duplicate semantics are unchanged.
    pub early_duplicates: u64,
    /// Successful batched accepted-event store commits (issue #143). Each tick
    /// of `flush_store_batch` that lands a non-empty batch bumps this by one —
    /// useful for capacity/health observations; the per-event accept counter
    /// remains [`accepted`](Self::accepted).
    pub store_insert_batches: u64,

    // -- silent-loss / fail-open evidence (fix 2) -------------------------------
    /// Unconfirmed admin tips expired by the attempt budget
    /// ([`max_unconfirmed_tip_attempts`](SyncConfig::max_unconfirmed_tip_attempts)).
    /// Each expiry *fails the removal-sensitive access gate open* on a tip that
    /// could never be confirmed; non-zero means at least one such fail-open
    /// happened this session, and the first one also records a CRITICAL
    /// `admin_tip_expired` [`TrustDecision`].
    pub suspect_tip_expired: u64,
}

/// One fold-accepted event whose `store.insert` failed, held in memory for
/// bounded per-tick retry (issue #119). The fold already committed the accept —
/// descendants will fold-accept and persist above the missing row — so the event
/// must not be dropped on the floor: retrying from here heals the hole locally
/// (the store's insert-time lamport propagation re-places the descendants),
/// without waiting for a peer to re-serve it (#118).
struct StoreRetry {
    event: ValidatedEvent,
    /// The peer the event arrived from (`None` for a local publish), so a
    /// late-succeeding insert still fans out to everyone but the sender.
    from: Option<PeerId>,
    /// Failed insert attempts so far, bounded by
    /// [`store_retry_attempts`](SyncConfig::store_retry_attempts).
    attempts: u32,
}

/// One parked orphan frame, held in memory pending backfill (spec D5/D7).
struct Parked {
    event: ValidatedEvent,
    author: IdentityKey,
    /// Arrival order, for oldest-first eviction.
    seq: u64,
    /// Backfill-chase depth, bounded by `max_backfill_depth`.
    depth: usize,
    /// The parents this frame is waiting on — persisted to `sync_parked_missing`
    /// and used to re-issue `WantEvents` after a restart (spec §6.3). A superset
    /// is safe: the retry filters by what the store already holds.
    missing: BTreeSet<EventId>,
}

/// An advertised-but-unconfirmed admin tip: a peer claims a higher `admin_seq`
/// than we hold. It drives [`AdminViewSuspect`](Completeness::AdminViewSuspect) so
/// we fail closed while catching up, but an [`AdminTip`](SyncMessage::AdminTip) is
/// an **unverified hint** — not proof the event exists — so it is bounded by an
/// attempt budget: a fabricated tip is expired after `attempts` ticks rather than
/// pinning the node fail-closed forever (spec D6 / §13). It is held only here and
/// is **never** folded into the held admin state (`admin_ids_by_seq`), so it can
/// neither forge a fork nor advance the local tip.
#[derive(Clone, Copy, Debug)]
struct SuspectTip {
    /// The advertised tip id (we may never hold it — it may not exist).
    id: EventId,
    /// The advertised `admin_seq`.
    seq: u64,
    /// Remaining catch-up ticks before this unconfirmed tip is expired.
    attempts: u32,
}

/// Bounded FIFO event-id dedup cache (issue #143 / #134 §22.2). The cache sits
/// **in front of** signature verification and any store work, so a replay
/// inside the cache window avoids Ed25519 verification and a `SQLite` insert
/// attempt entirely. Correctness never depends on it: a cache miss falls
/// through to the existing idempotent store path, and the cache is populated
/// only after the store proves an id is persisted ([`note_event_id_seen`](SyncEngine::note_event_id_seen)),
/// so an invalid first arrival cannot poison it.
///
/// The `BTreeSet` + `VecDeque` shape keeps `contains`/`insert` deterministic
/// for tests and simulation (no hash-map randomized iteration, R4). `cap == 0`
/// disables the early path (the supported rollback knob).
#[derive(Debug)]
struct EventIdDedupCache {
    cap: usize,
    set: BTreeSet<EventId>,
    order: VecDeque<EventId>,
}

impl EventIdDedupCache {
    /// Build an empty cache with capacity `cap`. `cap == 0` disables early
    /// dedup ([`contains`](Self::contains) always returns `false`).
    fn new(cap: usize) -> Self {
        Self {
            cap,
            set: BTreeSet::new(),
            order: VecDeque::with_capacity(cap.min(64)),
        }
    }

    /// Whether `id` is currently cached. Read-only and never touches `SQLite` —
    /// the whole point of the early path.
    fn contains(&self, id: &EventId) -> bool {
        self.cap > 0 && self.set.contains(id)
    }

    /// Record an id as seen, evicting the oldest insertion when at capacity.
    /// Idempotent: a re-insert of an already-present id does not duplicate it
    /// in `order` (so the eviction order stays the first-seen order).
    fn insert(&mut self, id: EventId) {
        if self.cap == 0 {
            return;
        }
        if !self.set.insert(id) {
            return;
        }
        while self.order.len() >= self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.set.remove(&evicted);
            } else {
                break;
            }
        }
        self.order.push_back(id);
    }

    /// Number of cached ids (test/observability helper; also used by the
    /// open-time seeding loop to respect the cap when iterating a `BTreeSet`).
    fn len(&self) -> usize {
        self.set.len()
    }
}

/// A buffered run of fold-accepted events awaiting a single batched
/// [`EventStore::insert_all_outcomes`] commit (issue #143). The `from` slot
/// preserves the per-event origin so a late-landing insert still fans out to
/// every connected peer except the sender (matching `store_and_fanout`).
struct PendingStoreBatch {
    events: Vec<ValidatedEvent>,
    from: Vec<Option<PeerId>>,
}

impl PendingStoreBatch {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            from: Vec::new(),
        }
    }

    fn push(&mut self, ev: ValidatedEvent, from: Option<PeerId>) {
        self.events.push(ev);
        self.from.push(from);
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Maximum retained log lines (bounded, in-memory; spec §9 / R4).
const MAX_LOG_LINES: usize = 256;

use super::message::{EVENTS_ENVELOPE_ALLOWANCE, EVENTS_PER_FRAME_OVERHEAD};

/// Bounded entries in the memoized `covered_cache` (deterministic eviction).
const COVERED_CACHE_ENTRIES: usize = 4;

/// The membership event types that are **never windowed** (spec §4.1).
const MEMBERSHIP_TYPES: [EventType; 5] = [
    EventType::RoomCreated,
    EventType::MemberInvited,
    EventType::MemberJoined,
    EventType::MemberLeft,
    EventType::MemberRemoved,
];

/// The deterministic bounded recent-sync engine for one room.
pub struct SyncEngine {
    room_id: RoomId,
    config: SyncConfig,
    store: EventStore,
    fold: RoomMembership,

    /// Currently-connected peers (deterministic fan-out order via `BTreeSet`).
    peers: BTreeSet<PeerId>,

    /// In-memory orphan park (spec D5/D7), keyed by event id.
    park: BTreeMap<EventId, Parked>,
    /// Fold-accepted events whose `store.insert` failed, awaiting bounded
    /// per-tick retry (issue #119), keyed by event id. Session-only on purpose:
    /// persisting it to the same failing store is circular, and a restart
    /// re-folds from `events` — clearing the fold/store divergence this queue
    /// exists to repair.
    store_retry: BTreeMap<EventId, StoreRetry>,
    /// Restored parked frames still owed a one-shot `WantEvents` re-issue on the
    /// first `on_connect`/`on_tick` after `open` (spec §6.3 — buffering **and
    /// retry** survive a restart). Ongoing retry thereafter is the anti-entropy
    /// pulls + [`wake_park`](Self::wake_park), exactly as for a freshly-parked frame.
    restored_backfill: BTreeSet<EventId>,
    /// Monotonic park arrival counter for eviction ordering.
    park_seq: u64,
    /// Depth to assign a frame that arrives in answer to our backfill (spec §6.2).
    backfill_depth: BTreeMap<EventId, usize>,
    /// Per-author backfill token buckets (spec §4.4).
    tokens: BTreeMap<IdentityKey, u32>,

    /// An advertised-but-unconfirmed admin tip ahead of our local held chain
    /// (spec D6). Held only here — never folded into the held admin state below —
    /// and bounded by an attempt budget so a fabricated higher tip cannot pin the
    /// node fail-closed forever (spec §13).
    suspect_tip: Option<SuspectTip>,
    /// Admin event ids seen at each `admin_seq`, recorded **only** from
    /// held-and-validated admin events (local accepts + the persisted chain),
    /// **never** from raw advertisements. Two distinct *held* ids at one seq is a
    /// genuine admin self-fork (spec §7); sourcing this from held events alone
    /// means a peer cannot forge a fork against the honest admin by advertising a
    /// fabricated tip.
    admin_ids_by_seq: BTreeMap<u64, BTreeSet<EventId>>,
    completeness: Completeness,
    fail_closed: BTreeSet<IdentityKey>,
    trust_decisions: Vec<TrustDecision>,

    counters: SyncCounters,
    logs: Vec<String>,
    /// Advisory-flag codes recorded on events accepted since the last
    /// [`take_flags`](Self::take_flags) drain (spec IR-0110 §5.9) — the
    /// receive-path source for `AuditSink::event_flagged`. Never affects the
    /// verdict, order, or any authz/expiry decision; purely observability.
    pending_flags: Vec<&'static str>,
    /// Events accepted (`InsertOutcome::Inserted`) since the last
    /// [`take_ingested`](Self::take_ingested) drain — the push-subscription feed
    /// (issue #83 / IR-0307). Mirrors `pending_flags`: the sans-IO engine cannot
    /// own a tokio broadcast sender, so it buffers and the net pump drains + fans
    /// out.
    pending_ingested: Vec<StoredEvent>,
    /// Memoized [`authorization_closure_ids`](Self::authorization_closure_ids),
    /// invalidated on every store insert. The closure walk is `O(events)` store
    /// lookups; without the cache a quiesced mesh would re-pay it per peer per
    /// tick for `serve_want_membership` (and an unauthenticated provisional
    /// dialer could spam `WantMembership` to force it).
    closure_cache: Option<BTreeSet<EventId>>,
    /// Memoized [`covered_by_claim`](Self::covered_by_claim) downsets, keyed by
    /// the exact claimed-and-held frontier and invalidated on every store insert
    /// (issue #113). Bounded to [`COVERED_CACHE_ENTRIES`] with deterministic
    /// smallest-key eviction (spec R4). The converged-mesh case normally never
    /// reaches this map — a claim containing every local head is answered by a
    /// fast path — so entries typically exist only while peers are catching up.
    covered_cache: BTreeMap<BTreeSet<EventId>, BTreeSet<EventId>>,
    /// Ticks seen since `open`, driving the claim's rotating window
    /// ([`membership_have`](Self::membership_have), issue #113). In-memory
    /// session state like the caches: a restart resets the sweep phase, never
    /// its guarantees (the window position is coverage-only, not trust).
    claim_rotation: u64,
    /// Whether the next tick should emit full anti-entropy pulls even if no local
    /// sync activity has happened since the previous tick.
    force_next_tick_pull: bool,

    /// Bounded early event-id dedup cache (issue #143): checked before signature
    /// verification and any store work, so a replay inside the cache window is a
    /// cheap no-op. `cap == 0` disables the early path; correctness always rests
    /// on the store's primary-key idempotency.
    dedup_cache: EventIdDedupCache,
    /// Buffered fold-accepted events awaiting one batched store commit
    /// (issue #143). Flushed at every delivery boundary so consecutive accepted
    /// events amortize `SQLite` transaction overhead without reordering observable
    /// post-commit side effects.
    pending_batch: PendingStoreBatch,
}

/// How much of the responder's held set a `WantMembership` `have` ancestry
/// claim covers (issue #113; see [`SyncEngine::covered_by_claim`]).
enum Coverage {
    /// The claim contains every local DAG head, so its downset covers the entire
    /// held set — nothing to serve.
    Everything,
    /// The downset of the claimed-and-held ids.
    Set(BTreeSet<EventId>),
}

impl SyncEngine {
    /// Open the engine over an existing store for one room, rebuilding the fold
    /// from the persisted `events` and re-deriving the admin tip (spec §9 restart
    /// determinism; the store holds exactly the fold-accepted set, so this is
    /// lossless for steady state).
    ///
    /// # Errors
    /// [`SyncError::Config`] if `config` is invalid, or [`SyncError::Store`] on a
    /// store read failure.
    pub fn open(store: EventStore, room_id: RoomId, config: SyncConfig) -> Result<Self, SyncError> {
        config.validate().map_err(SyncError::Config)?;

        // Rebuild the fold from the authoritative table. Every stored event is
        // fold-accepted hence causally complete (spec D5), so `room_tail` with no
        // practical limit returns the whole validated set in canonical order; the
        // fold is order-independent.
        let ctx = ValidationContext::for_room(room_id);
        let stored = store.room_tail(&room_id, u32::MAX)?;
        let mut validated = Vec::with_capacity(stored.len());
        for se in stored {
            // Re-validating persisted bytes cannot fail (they were validated
            // before storage); treat a failure as store corruption.
            let ev = validate_wire_bytes(&se.wire.to_bytes(), &ctx)
                .map_err(|r| SyncError::Store(StoreError::Decode(r)))?;
            validated.push(ev);
        }
        let fold = RoomMembership::from_events(room_id, validated);

        // Seed the early dedup cache (issue #143) from the persisted room ids.
        // A restart re-covers the recent window without paying store duplicate
        // paths on a flood of immediate replays. The seed order is bytewise from
        // the `BTreeSet` (not recency-based) — acceptable, this is a performance
        // guardrail, not correctness state.
        let mut dedup_cache = EventIdDedupCache::new(config.early_event_id_dedup_cache_entries);
        if dedup_cache.cap > 0 {
            if let Ok(ids) = store.room_event_ids(&room_id) {
                for id in ids {
                    dedup_cache.insert(id);
                    if dedup_cache.len() >= dedup_cache.cap {
                        break;
                    }
                }
            }
        }

        let mut engine = Self {
            room_id,
            config,
            store,
            fold,
            peers: BTreeSet::new(),
            park: BTreeMap::new(),
            store_retry: BTreeMap::new(),
            restored_backfill: BTreeSet::new(),
            park_seq: 0,
            backfill_depth: BTreeMap::new(),
            tokens: BTreeMap::new(),
            suspect_tip: None,
            admin_ids_by_seq: BTreeMap::new(),
            completeness: Completeness::Complete,
            fail_closed: BTreeSet::new(),
            trust_decisions: Vec::new(),
            counters: SyncCounters::default(),
            logs: Vec::new(),
            pending_flags: Vec::new(),
            pending_ingested: Vec::new(),
            closure_cache: None,
            covered_cache: BTreeMap::new(),
            claim_rotation: 0,
            force_next_tick_pull: true,
            dedup_cache,
            pending_batch: PendingStoreBatch::new(),
        };
        engine.seed_admin_state()?;
        // Restore the genuinely non-rebuildable transient state (the orphan park,
        // the unconfirmed admin-tip suspicion, the backfill token buckets, and the
        // trust-decision audit) from the v2 sync-cache tables BEFORE recomputing
        // completeness, so a persisted suspicion re-arms the fail-closed gate and a
        // reboot cannot fail-open (spec §6.1 / D3).
        engine.restore_persisted_state()?;
        engine.recompute_completeness()?;
        Ok(engine)
    }

    /// The room this engine reconciles.
    #[must_use]
    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// This engine's config (crate-internal). Used only by the [`SimNet`](super::sim::SimNet)
    /// restart helper to re-`open` with the same bounds; **not** part of the
    /// public surface (spec §7).
    pub(crate) fn config(&self) -> SyncConfig {
        self.config
    }

    /// Consume the engine and return its owned [`EventStore`] (crate-internal).
    /// The [`SimNet`](super::sim::SimNet) restart helper re-`open`s a fresh engine
    /// over the *same* store to model a process restart — the store's `events` and
    /// v2 sync-cache tables persist, only the in-memory session state is dropped
    /// (spec D9 / AC5). **Not** part of the public surface (spec §7).
    pub(crate) fn into_store(self) -> EventStore {
        self.store
    }

    /// Test-only mutable access to the owned store, so the #119 tests can arm
    /// the store's deterministic insert fault injection mid-scenario.
    #[cfg(test)]
    pub(crate) fn store_mut(&mut self) -> &mut EventStore {
        &mut self.store
    }

    // ------------------------------------------------------------------
    // Entry points (§6)
    // ------------------------------------------------------------------

    /// Ingest one inbound/fetched `WireEvent` frame (§6.1). Returns frames to send.
    /// A per-frame validation failure is a logged drop, never an error.
    pub fn ingest_frame(&mut self, from: PeerId, bytes: &[u8]) -> Vec<Outgoing> {
        let mut out = Vec::new();
        self.deliver_bytes(Some(from), bytes, &mut out);
        // Issue #143: flush any buffered accept and run parked-frame promotion
        // before returning, so post-commit side effects land in order.
        self.finalize_delivery(&mut out);
        out
    }

    /// Publish a locally-authored, stateless-valid frame (§6.5): ingest it and, on
    /// accept, fan it out to every connected peer.
    ///
    /// # Errors
    /// [`SyncError::InvalidFrame`] if the bytes fail stateless validation, or
    /// [`SyncError::OversizedFrame`] if the frame could never fit a wire frame
    /// even as a single-frame `Events` message (a locally-authored event several
    /// content fields of which are unbounded could otherwise enter the log yet be
    /// undeliverable to every peer — permanent silent divergence, issue #113).
    pub fn publish(&mut self, bytes: &[u8]) -> Result<Vec<Outgoing>, SyncError> {
        if bytes
            .len()
            .saturating_add(EVENTS_ENVELOPE_ALLOWANCE + EVENTS_PER_FRAME_OVERHEAD)
            > super::message::MAX_FRAME_BYTES
        {
            return Err(SyncError::OversizedFrame {
                frame_len: bytes.len(),
            });
        }
        let ctx = ValidationContext::for_room(self.room_id);
        let ev = validate_wire_bytes(bytes, &ctx).map_err(SyncError::InvalidFrame)?;
        let mut out = Vec::new();
        self.deliver(ev, None, &mut out);
        // Issue #143: flush + wake_park before returning.
        self.finalize_delivery(&mut out);
        Ok(out)
    }

    /// A peer link came up (§6.3): advertise our admin tip + heads and request the
    /// never-windowed membership sub-DAG and the bounded recent chat window.
    pub fn on_connect(&mut self, peer: PeerId) -> Vec<Outgoing> {
        self.peers.insert(peer);
        self.force_next_tick_pull = true;
        let mut out = vec![
            to(peer, self.admin_tip_msg()),
            to(peer, self.heads_msg()),
            to(
                peer,
                SyncMessage::WantMembership {
                    room_id: self.room_id,
                    have: self.membership_have(),
                },
            ),
            to(
                peer,
                SyncMessage::WantRecentChat {
                    room_id: self.room_id,
                    window: Window {
                        max_count: self.config.chat_window_default,
                        since_ms: None,
                    },
                    have: self.chat_have(),
                },
            ),
        ];
        // Re-issue by-id backfill for any park restored from disk (spec §6.3), so a
        // valid-but-early frame survives a crash, not only a transport reconnect.
        self.retry_restored_park(&mut out);
        out
    }

    /// A peer link went down: stop fanning out to it. The orphan park is retained
    /// for retry on reconnect (spec §6.3 / A4).
    pub fn on_disconnect(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
        self.force_next_tick_pull = true;
    }

    /// Handle one inbound control/data message (§6.4 responder + the detector).
    pub fn on_message(&mut self, from: PeerId, msg: SyncMessage) -> Vec<Outgoing> {
        let mut out = Vec::new();
        self.force_next_tick_pull = true;
        if msg.room_id() != &self.room_id {
            self.log("dropped frame for foreign room");
            return out;
        }
        match msg {
            SyncMessage::AdminTip { tip, .. } => self.handle_admin_tip(tip, &mut out),
            SyncMessage::Heads { heads, .. } => self.handle_heads(&heads),
            SyncMessage::WantEvents { ids, .. } => self.serve_want_events(from, &ids, &mut out),
            SyncMessage::WantMembership { have, .. } => {
                self.serve_want_membership(from, &have, &mut out);
            }
            SyncMessage::WantRecentChat { window, have, .. } => {
                self.serve_want_recent_chat(from, window, &have, &mut out);
            }
            SyncMessage::Events { frames, .. } => {
                // Issue #143: consecutive accepted frames accumulate into the
                // pending batch and are flushed as one store commit at the end
                // of the loop, so N consecutive accepted events commit in
                // ⌈N/batch⌉ transactions rather than N.
                for frame in frames {
                    self.deliver_bytes(Some(from), &frame, &mut out);
                }
            }
            SyncMessage::NotFound { ids, .. } => {
                self.log(&format!("peer lacks {} requested ids", ids.len()));
            }
            // A join-bootstrap capability proof (issue #112) is a transport-layer
            // concern: the network adapter verifies it (via `capability_proof_matches`)
            // and gates the provisional membership-closure serve on it, before this
            // point. The deterministic engine treats it as a no-op so a forwarded or
            // replayed proof never affects the validated set or convergence.
            SyncMessage::ProveCapability { .. } => {}
        }
        // Issue #143: flush the batched accepted events and run parked-frame
        // promotion before returning, so post-commit side effects land in
        // order and the transport adapter observes a consistent store state.
        self.finalize_delivery(&mut out);
        out
    }

    /// Periodic tick (§6.3 retry path): refill backfill tokens, retry the orphan
    /// park, re-advertise the admin tip, and re-issue the never-windowed
    /// membership pull + the bounded chat pull as anti-entropy. The re-pull closes
    /// the one gap a shuffled handshake can open: a chat frame delivered *before*
    /// the membership frame that names its author is dropped at the signer
    /// pre-check (§6.2), and only a re-pull — once membership is established —
    /// recovers it. The pulls carry `have` lists, so a converged peer's responses
    /// are empty and the mesh quiesces. `now_ms` is advisory (token refill is
    /// per-call, not clock-driven, to stay deterministic — spec R4).
    pub fn on_tick(&mut self, _now_ms: u64) -> Vec<Outgoing> {
        let mut out = Vec::new();
        // Advance the claim's rotating window (issue #113) — per tick, not per
        // peer, so every peer sees the same claim this round (determinism, R4).
        self.claim_rotation = self.claim_rotation.wrapping_add(1);
        self.refill_tokens();
        self.expire_suspect_tip();
        // Retry failed inserts first (issue #119): a healed hole re-places its
        // stored descendants, so this tick's `have` claims can already cover them.
        self.retry_store(&mut out);
        self.retry_park(&mut out);
        // A restart may have left parked frames owed their one-shot by-id backfill;
        // the first tick after `open` re-issues it if no `on_connect` did (spec §6.3).
        self.retry_restored_park(&mut out);
        let emit_pulls = self.force_next_tick_pull || self.has_pending_sync_work();
        self.force_next_tick_pull = false;
        let membership_have = emit_pulls.then(|| self.membership_have());
        let chat_have = emit_pulls.then(|| self.chat_have());
        for peer in self.peers.iter().copied().collect::<Vec<_>>() {
            out.push(to(peer, self.admin_tip_msg()));
            if let Some(have) = &membership_have {
                out.push(to(
                    peer,
                    SyncMessage::WantMembership {
                        room_id: self.room_id,
                        have: have.clone(),
                    },
                ));
            }
            if let Some(have) = &chat_have {
                out.push(to(
                    peer,
                    SyncMessage::WantRecentChat {
                        room_id: self.room_id,
                        window: Window {
                            max_count: self.config.chat_window_default,
                            since_ms: None,
                        },
                        have: have.clone(),
                    },
                ));
            }
        }
        out
    }

    fn has_pending_sync_work(&self) -> bool {
        !self.park.is_empty()
            || !self.store_retry.is_empty()
            || !self.restored_backfill.is_empty()
            || self.suspect_tip.is_some()
    }

    // ------------------------------------------------------------------
    // Queries (CLI / planes / tests)
    // ------------------------------------------------------------------

    /// The current convergent membership snapshot.
    #[must_use]
    pub fn snapshot(&self) -> MembershipSnapshot {
        self.fold.snapshot()
    }

    /// The most-recent `limit` causally-placed events in canonical
    /// `(lamport, event_id)` order — the deterministic display timeline
    /// (Membership §2). A thin read passthrough to
    /// [`EventStore::room_tail`](crate::store::EventStore::room_tail) so a running
    /// node can surface its room timeline for display without a second store handle.
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read failure.
    pub fn room_tail(&self, limit: u32) -> Result<Vec<StoredEvent>, SyncError> {
        Ok(self.store.room_tail(&self.room_id, limit)?)
    }

    /// The current DAG heads of the room — the `prev_events` a freshly authored
    /// event must cite. A thin read passthrough to
    /// [`EventStore::heads`](crate::store::EventStore::heads) so a running node can
    /// author a follow-on event (e.g. a `pipe.opened`) without a second store handle.
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read failure.
    pub fn heads(&self) -> Result<Vec<EventId>, SyncError> {
        Ok(self.store.heads(&self.room_id)?)
    }

    /// The governing `pipe.opened` for `pipe_id` from the **validated** set, or
    /// `None` if no such pipe is known locally (the Pipe plane's stage-2 lookup,
    /// spec §6.5.1). Read-only over the store, like [`room_tail`](Self::room_tail);
    /// the access decision against the **current** snapshot belongs to the caller.
    ///
    /// On the (cryptographically improbable) event of two `pipe.opened` sharing a
    /// `pipe_id`, the lowest-`event_id` one is returned for determinism.
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read or decode failure.
    pub fn pipe_opened(
        &self,
        pipe_id: &[u8; crate::event::constants::SHORT_ID_LEN],
    ) -> Result<Option<crate::event::content::PipeOpened>, SyncError> {
        // `by_type` is ordered `(lamport, event_id)`, so the first match is the
        // deterministic governing event.
        for se in self.store.by_type(&self.room_id, EventType::PipeOpened)? {
            let event = crate::event::signed::SignedEvent::decode(&se.wire.signed)
                .map_err(|r| SyncError::Store(StoreError::Decode(r)))?;
            if let Content::PipeOpened(opened) = event.content {
                if &opened.pipe_id == pipe_id {
                    return Ok(Some(opened));
                }
            }
        }
        Ok(None)
    }

    /// Whether a `pipe.closed` for `pipe_id` is present in the validated set (the
    /// `pipe.closed`-causally-known check the Pipe plane composes with the gate,
    /// spec §5). Fail-closed: any decode failure surfaces as an error the caller
    /// treats as "closed".
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read or decode failure.
    pub fn pipe_is_closed(
        &self,
        pipe_id: &[u8; crate::event::constants::SHORT_ID_LEN],
    ) -> Result<bool, SyncError> {
        for se in self.store.by_type(&self.room_id, EventType::PipeClosed)? {
            let event = crate::event::signed::SignedEvent::decode(&se.wire.signed)
                .map_err(|r| SyncError::Store(StoreError::Decode(r)))?;
            if let Content::PipeClosed(closed) = event.content {
                if &closed.pipe_id == pipe_id {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// The set of BLAKE3-256 blob hashes referenced by a valid `file.shared` in the
    /// validated set — the Blob Plane serve gate's per-hash authorization source
    /// (IR-0204 spec §5.3). Read-only over the store, like [`pipe_opened`](Self::pipe_opened);
    /// the gate decision itself belongs to the caller.
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read or decode failure.
    pub fn file_shared_hashes(&self) -> Result<BTreeSet<[u8; 32]>, SyncError> {
        let mut hashes = BTreeSet::new();
        for se in self.store.by_type(&self.room_id, EventType::FileShared)? {
            let event = crate::event::signed::SignedEvent::decode(&se.wire.signed)
                .map_err(|r| SyncError::Store(StoreError::Decode(r)))?;
            if let Content::FileShared(f) = event.content {
                hashes.insert(*f.blob_hash.as_bytes());
            }
        }
        Ok(hashes)
    }

    /// Whether `secret` proves possession of an on-log invite `invite_id` in this
    /// room: the recomputed capability hash matches an accepted `member.invited`
    /// (issue #112 — the join-bootstrap capability proof). The network adapter
    /// calls this to gate the never-windowed membership **closure** serve to a
    /// *provisional* join-bootstrap peer: since PR #111 that closure can carry chat
    /// that entered the membership ancestry, and an uninvited dialer must not pull
    /// it. A dialer that holds a minted invite secret can; a stranger cannot.
    ///
    /// This is a bootstrap **privacy** gate, not an authorization one — it does not
    /// check expiry, consumption, role, or the dialer's identity (an invite that was
    /// once minted still proves "was invited", which is the bar for *seeing* room
    /// history). The convergent authorization authority remains `gate_join`, run on
    /// the actual join by every peer, and is unchanged.
    #[must_use]
    pub fn capability_proof_matches(
        &self,
        invite_id: &[u8; crate::event::constants::SHORT_ID_LEN],
        secret: &[u8; crate::event::constants::SHORT_ID_LEN],
    ) -> bool {
        let expected = crate::event::content::capability_hash(&self.room_id, invite_id, secret);
        let Ok(invites) = self.store.by_type(&self.room_id, EventType::MemberInvited) else {
            return false;
        };
        invites.iter().any(|se| {
            crate::event::signed::SignedEvent::decode(&se.wire.signed)
                .ok()
                .and_then(|ev| match ev.content {
                    Content::MemberInvited(inv) => Some(inv),
                    _ => None,
                })
                .is_some_and(|inv| inv.invite_id == *invite_id && inv.capability_hash == expected)
        })
    }

    /// The admin-completeness verdict the access planes consult (spec D6).
    #[must_use]
    pub fn completeness(&self) -> Completeness {
        self.completeness
    }

    /// The subjects on which removal-sensitive access must fail closed while
    /// [`completeness`](Self::completeness) is not [`Complete`](Completeness::Complete).
    #[must_use]
    pub fn fail_closed_subjects(&self) -> Vec<IdentityKey> {
        self.fail_closed.iter().copied().collect()
    }

    /// The set-equality / convergence oracle (spec D8).
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read failure.
    pub fn digest(&self) -> Result<SyncDigest, SyncError> {
        Ok(SyncDigest {
            event_ids: self.store.room_event_ids(&self.room_id)?,
            admin_tip: self.store.admin_chain_tip(&self.room_id)?,
            snapshot: self.fold.snapshot(),
        })
    }

    /// The never-windowed **authorization-class** id set (the five membership
    /// types plus every admin-authored event, spec §4.1) — the subset whose
    /// convergence is *unconditional* (independent of any chat window). Used by
    /// the windowed-case convergence assertion and the CLI audit surface.
    ///
    /// # Errors
    /// [`SyncError::Store`] on a store read failure.
    pub fn membership_event_ids(&self) -> Result<BTreeSet<EventId>, SyncError> {
        Ok(self.authorization_class_ids()?)
    }

    /// The recorded trust decisions (CRITICAL `equivocation` on an admin fork).
    #[must_use]
    pub fn trust_decisions(&self) -> &[TrustDecision] {
        &self.trust_decisions
    }

    /// The Gate-D evidence counters (spec §9).
    #[must_use]
    pub fn counters(&self) -> SyncCounters {
        self.counters
    }

    /// The bounded in-memory drop/cap log (spec §4.4 — no silent truncation).
    #[must_use]
    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    /// Drain the advisory-flag codes recorded on events accepted since the last
    /// call (spec IR-0110 §5.9). Each call site (the `Node` receive-path pump)
    /// gets exactly the flags raised since it last drained.
    pub fn take_flags(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.pending_flags)
    }

    /// Drain the events accepted since the last call — the push-subscription
    /// feed for `Node::room_events` (issue #83 / IR-0307). Each freshly-Inserted
    /// event appears exactly once across own-publish, peer-sync, and delayed
    /// park-promotion; a duplicate re-see never lands here. Callers get exactly
    /// the events accepted since they last drained (destructive `mem::take`).
    ///
    /// NOTE: within a single drive, park-promotion appends in engine-iteration
    /// order, NOT causal/Lamport order (see spec §6.2). Set-membership + exactly-once
    /// hold; strict ordering does not.
    pub fn take_ingested(&mut self) -> Vec<StoredEvent> {
        std::mem::take(&mut self.pending_ingested)
    }

    /// The number of frames currently parked (test/observability helper).
    #[must_use]
    pub fn parked_len(&self) -> usize {
        self.park.len()
    }

    /// The number of fold-accepted events awaiting a store-insert retry
    /// (issue #119; test/observability helper). Non-zero means the local store
    /// is currently failing writes and the engine is holding the affected
    /// events for its bounded per-tick retry.
    #[must_use]
    pub fn store_retry_len(&self) -> usize {
        self.store_retry.len()
    }

    /// The number of events tracked by the underlying membership fold — accepted,
    /// buffered, or rejected (test/observability helper). Non-member junk dropped
    /// at the §6.2 pre-gate never reaches the fold, so a non-member flood leaves
    /// this unchanged: the Gate-D anti-amplification bound holds in the fold too,
    /// not only in the engine park (spec §6.2 step 1 / D5).
    #[must_use]
    pub fn fold_tracked_len(&self) -> usize {
        self.fold.tracked_event_count()
    }

    // ------------------------------------------------------------------
    // Local ingest (§6.1) + anti-amplification (§6.2)
    // ------------------------------------------------------------------

    fn deliver_bytes(&mut self, from: Option<PeerId>, bytes: &[u8], out: &mut Vec<Outgoing>) {
        // Issue #143 / #134 §22.2: cheap pre-validation parse + early event-id
        // dedup. Decoding the outer `WireEvent` and recomputing the id from
        // `wire.signed` (never trusting the advisory `wire.id`) is far cheaper
        // than Ed25519 verification or a SQLite insert; a cache hit on a known
        // id short-circuits both. A malformed envelope or an id mismatch is
        // still a counted+logged reject (the cache lookup never runs on a frame
        // the safe parser refuses). See `prevalidate_event_id`.
        match prevalidate_event_id(bytes) {
            Ok(event_id) => {
                if self.dedup_cache.contains(&event_id) {
                    self.counters.early_duplicates += 1;
                    return;
                }
            }
            Err(reason) => {
                self.counters.rejected += 1;
                self.log(&format!("reject.{}", reason.code()));
                return;
            }
        }
        let ctx = ValidationContext::for_room(self.room_id);
        match validate_wire_bytes(bytes, &ctx) {
            Ok(ev) => self.deliver(ev, from, out),
            Err(reason) => {
                // A stateless-invalid frame (bad signature, non-canonical, …) is a
                // logged, counted drop — never stored, never fanned out (AC3). The
                // stable `reject.<code>` line is the CLI-visible signal under the
                // no-tracing-subscriber constraint (spec D8).
                self.counters.rejected += 1;
                self.log(&format!("reject.{}", reason.code()));
            }
        }
    }

    fn deliver(&mut self, ev: ValidatedEvent, from: Option<PeerId>, out: &mut Vec<Outgoing>) {
        // Anti-amplification pre-gate (§6.2 step 1): a frame whose parents are not
        // all present in the local validated set *would* buffer, and the fold
        // retains every event it ingests with no eviction. So before letting such a
        // frame into the fold, require its signer be plausibly in the room.
        // Otherwise a non-member flood citing distinct phantom parents would grow
        // the fold's node map unboundedly — the explicit Gate-D NO-GO — even though
        // the engine park drops them. Junk is dropped here: never folded, never
        // parked (spec §6.2 step 1 / D5: "dropped early, never parked").
        if self.would_buffer(&ev) && !self.signer_plausible(&ev) {
            self.counters.signer_dropped += 1;
            self.log("dropped frame pre-fold: anti_amplification_signer");
            return;
        }
        match self.fold.ingest(ev.clone()) {
            crate::membership::Ingest::Accepted { .. } => {
                // The chase for this id (if any) is over; clearing the depth entry
                // keeps the map bounded and lets a once-depth-dropped id be
                // re-delivered cleanly when it later arrives causally complete.
                self.backfill_depth.remove(&ev.event_id);
                // Issue #143: buffer the accepted event for a batched store
                // commit instead of persisting immediately. The flush happens
                // at deterministic boundaries — batch-size cap, end of the
                // current frame loop, end of the entry point — so N consecutive
                // accepted events commit in ⌈N/batch⌉ transactions, not N.
                self.pending_batch.push(ev, from);
                if self.pending_batch.len() >= self.config.store_insert_batch_size {
                    self.flush_store_batch(out);
                }
                // NOTE: wake_park is no longer driven from inside `deliver`. It
                // now runs at entry-point boundaries via `finalize_delivery`,
                // so a consecutive run of accepted events accumulates into a
                // single batch (spec D6 — the supported first implementation).
            }
            crate::membership::Ingest::Buffered { missing, .. } => {
                // Flush any pending accepts BEFORE emitting backfill: prior
                // accepts must commit so their fanout does not leapfrog this
                // buffered frame's response (spec D6 flush boundary).
                self.flush_store_batch(out);
                self.on_buffered(ev, from, &missing, out);
            }
            crate::membership::Ingest::Rejected { reason, .. } => {
                // Fold-rejected (non-member, bad capability, …): counted + logged
                // with a stable `reject.<code>`, stored nowhere, fanned out nowhere
                // (AC3 / spec D8).
                self.backfill_depth.remove(&ev.event_id);
                self.flush_store_batch(out);
                self.counters.rejected += 1;
                self.log(&format!("reject.{}", reason.code()));
            }
        }
    }

    /// Flush any buffered accepted events as one batched store commit, then
    /// promote parked frames the fold has since accepted, then flush again —
    /// the deterministic delivery wrap-up every entry point runs before
    /// returning its `Outgoing` frames (issue #143 / spec D6). Centralizing
    /// the wake-park here (instead of inside `deliver`) is what lets a run of
    /// consecutive accepted frames accumulate into one batch.
    fn finalize_delivery(&mut self, out: &mut Vec<Outgoing>) {
        self.flush_store_batch(out);
        self.wake_park(out);
        // `wake_park` promotes through the per-event `store_and_fanout`, which
        // commits inline — no batched residue — but call once more in case a
        // future change routes parked promotions through the batch too.
        self.flush_store_batch(out);
    }

    /// Commit the buffered accepted events in one `SQLite` transaction and apply
    /// per-event post-commit side effects in input order (issue #143 / spec D7).
    /// On a batch failure every affected event enters the bounded #119 retry
    /// path and no side effect runs — the fold/store divergence is healed on a
    /// later tick or by a peer re-serve, exactly like a failed single insert.
    fn flush_store_batch(&mut self, out: &mut Vec<Outgoing>) {
        if self.pending_batch.is_empty() {
            return;
        }
        let batch = std::mem::replace(&mut self.pending_batch, PendingStoreBatch::new());
        self.force_next_tick_pull = true;
        match self.store.insert_all_outcomes(&batch.events) {
            Ok(outcomes) => {
                self.counters.store_insert_batches += 1;
                for ((ev, from), outcome) in batch
                    .events
                    .iter()
                    .zip(batch.from.iter().copied())
                    .zip(outcomes)
                {
                    // A successful insert supersedes any pending retry for this
                    // id (a peer may have re-served an event whose first insert
                    // failed, issue #119 — `peer_reserve_clears_pending_retry_exactly_once`).
                    self.store_retry.remove(&ev.event_id);
                    self.apply_insert_outcome(outcome, ev, from, out);
                }
            }
            Err(e) => {
                // All-or-nothing: `insert_all_outcomes` ran in one transaction,
                // so a failure means no event in the batch is durably stored.
                // Apply NO side effects (spec D8); enqueue retry for each in
                // input order; log distinctly from a per-event insert failure
                // while keeping the legacy `store insert failed` substring the
                // existing #119 tests assert on (no silent degradation).
                let n = batch.events.len() as u64;
                self.counters.store_insert_failed += n;
                self.log(&format!("store insert failed (batch): {e}"));
                for (ev, from) in batch.events.iter().zip(batch.from.iter().copied()) {
                    self.enqueue_store_retry(ev, from);
                }
            }
        }
    }

    /// Persist an accepted event, update admin state, and fan it out. Does **not**
    /// wake the park (the caller drives that loop to avoid recursion).
    ///
    /// The fold has already committed the accept when this runs, so a failed
    /// insert must not silently drop the event — the fold and the store would
    /// disagree for the rest of the session, and descendants (whose readiness
    /// checks the fold) would persist above a permanently missing row (issue
    /// #119). The event is queued for bounded per-tick retry instead
    /// ([`retry_store`](Self::retry_store)); fan-out, counters, and the push
    /// feed are deferred until the insert actually lands, so nothing is
    /// announced that this node cannot serve.
    ///
    /// This is the per-event path, used by `wake_park` for parked-frame
    /// promotion (spec D6 — the first implementation keeps parked promotions
    /// per-event; the direct accepted-event path goes through `flush_store_batch`).
    fn store_and_fanout(
        &mut self,
        ev: &ValidatedEvent,
        from: Option<PeerId>,
        out: &mut Vec<Outgoing>,
    ) {
        self.force_next_tick_pull = true;
        let outcome = match self.store.insert(ev) {
            Ok(o) => o,
            Err(e) => {
                self.counters.store_insert_failed += 1;
                self.log(&format!("store insert failed: {e}"));
                self.enqueue_store_retry(ev, from);
                return;
            }
        };
        // A successful insert supersedes any pending retry for this id (a peer
        // may have re-served an event whose first insert failed, issue #119).
        self.store_retry.remove(&ev.event_id);
        self.apply_insert_outcome(outcome, ev, from, out);
    }

    /// Record an event id in the early dedup cache (issue #143). Called only
    /// after the store has proven the id is persisted — on a successful
    /// `Inserted` or an idempotent `Duplicate` — so a bad-signature first
    /// arrival can never poison the cache and suppress a later valid copy
    /// (spec D2). The cache is a performance guardrail; correctness still
    /// rests on the store's primary-key idempotency.
    fn note_event_id_seen(&mut self, id: EventId) {
        self.dedup_cache.insert(id);
    }

    /// Apply the bookkeeping for a **successful** `store.insert`: counters,
    /// cache invalidation, admin fork-detection state, the push-subscription
    /// feed, advisory flags, fan-out, and the completeness recompute. Shared by
    /// the batched direct path ([`flush_store_batch`](Self::flush_store_batch)),
    /// the per-event parked-promotion path
    /// ([`store_and_fanout`](Self::store_and_fanout)), and the deferred
    /// insert-retry path ([`retry_store`](Self::retry_store), issue #119), so a
    /// late-landing event gets exactly the treatment an immediate one does.
    fn apply_insert_outcome(
        &mut self,
        outcome: InsertOutcome,
        ev: &ValidatedEvent,
        from: Option<PeerId>,
        out: &mut Vec<Outgoing>,
    ) {
        let id = ev.event_id;
        // Issue #143: the store has proven `id` is persisted (Inserted OR
        // idempotent Duplicate), so the early dedup cache can safely record it
        // without risking poisoning by an invalid first arrival (spec D2).
        self.note_event_id_seen(id);
        match outcome {
            InsertOutcome::Duplicate => {
                self.counters.duplicates += 1;
                // Idempotent re-see: no state change, no re-broadcast storm
                // (spec §8 duplicate-idempotency vector).
            }
            InsertOutcome::Inserted => {
                self.counters.accepted += 1;
                // The validated set grew: the memoized authorization closure is
                // stale, and a memoized covered-downset may under-cover (a
                // claimed id we lacked — skipped then — may now be held, and a
                // healed hole lets the walk reach deeper). Under-covering only
                // over-serves, but pinning it forever would leave a converged
                // peer re-serving duplicates every tick, so both caches reset.
                self.closure_cache = None;
                self.covered_cache.clear();
                self.note_admin_event(id);
                // Push-subscription feed (issue #83): emit exactly once, only on a real
                // insert (the Duplicate arm never reaches here → exactly-once for free).
                match self.store.get_in_room(&self.room_id, &id) {
                    Ok(Some(stored)) => self.pending_ingested.push(stored),
                    Ok(None) => self.log("room_events: inserted event vanished from store"),
                    Err(e) => self.log(&format!("room_events: store.get failed: {e}")),
                }
                // Advisory flags on a freshly-accepted event (spec IR-0110 §5.9,
                // e.g. `clock_skew`) — never re-raised for a duplicate re-see.
                for flag in &ev.flags {
                    self.log(&format!("flag.{}", flag.code()));
                    self.pending_flags.push(flag.code());
                }
                // Fan out to every connected peer except the sender.
                let bytes = ev.wire.to_bytes();
                for peer in self.peers.iter().copied().collect::<Vec<_>>() {
                    if Some(peer) != from {
                        out.push(Outgoing {
                            peer,
                            msg: SyncMessage::Events {
                                room_id: self.room_id,
                                frames: vec![bytes.clone()],
                            },
                        });
                        self.counters.frames_sent += 1;
                    }
                }
                if let Err(e) = self.recompute_completeness() {
                    self.log(&format!("completeness recompute failed: {e}"));
                }
            }
        }
    }

    /// Queue a fold-accepted event whose insert failed for per-tick retry
    /// (issue #119). A re-see of an already-queued id keeps the existing entry
    /// (and its attempt count): duplicate deliveries are not extra retries.
    /// Beyond [`max_store_retry_total`](SyncConfig::max_store_retry_total) the
    /// event is abandoned immediately — logged, counted, and surfaced as a
    /// CRITICAL `store_degraded` decision (no silent truncation, spec §4.4).
    fn enqueue_store_retry(&mut self, ev: &ValidatedEvent, from: Option<PeerId>) {
        let id = ev.event_id;
        if self.store_retry.contains_key(&id) {
            return;
        }
        if self.store_retry.len() >= self.config.max_store_retry_total {
            self.counters.store_retry_dropped += 1;
            self.log("store retry dropped: store_retry_total");
            self.record_store_degraded(id);
            return;
        }
        self.store_retry.insert(
            id,
            StoreRetry {
                event: ev.clone(),
                from,
                attempts: 0,
            },
        );
    }

    /// Retry every queued failed insert (issue #119, one attempt per event per
    /// tick). A success runs the full deferred-accept bookkeeping — the event
    /// is counted, fed to subscribers, and fanned out only now, and the store's
    /// insert-time lamport propagation re-places any descendants stored above
    /// the hole. Exhausting the attempt budget abandons the event with a
    /// CRITICAL `store_degraded` decision; the fold keeps it Accepted (the
    /// verdict is ancestor-stable and cannot be rolled back), so the fold/store
    /// divergence persists until a peer re-serves the event (#118) or a restart
    /// re-folds from `events`.
    fn retry_store(&mut self, out: &mut Vec<Outgoing>) {
        for id in self.store_retry.keys().copied().collect::<Vec<_>>() {
            let Some((ev, from)) = self.store_retry.get(&id).map(|r| (r.event.clone(), r.from))
            else {
                continue;
            };
            match self.store.insert(&ev) {
                Ok(outcome) => {
                    self.store_retry.remove(&id);
                    self.apply_insert_outcome(outcome, &ev, from, out);
                }
                Err(e) => {
                    self.counters.store_insert_failed += 1;
                    self.log(&format!("store insert retry failed: {e}"));
                    let exhausted = self.store_retry.get_mut(&id).map_or(true, |entry| {
                        entry.attempts += 1;
                        entry.attempts >= self.config.store_retry_attempts
                    });
                    if exhausted {
                        self.store_retry.remove(&id);
                        self.counters.store_retry_dropped += 1;
                        self.log("store retry dropped: store_retry_attempts");
                        self.record_store_degraded(id);
                    }
                }
            }
        }
    }

    /// Surface a CRITICAL `store_degraded` [`TrustDecision`] (issue #119): a
    /// fold-accepted event could not be persisted and its retry budget is gone.
    /// The operator-facing meaning is "this node's store is dropping writes" —
    /// the fold counts and fans out state the store cannot serve, until a peer
    /// re-serve (#118) or a restart re-fold clears the divergence. Persisting
    /// the decision may itself fail on the degraded store; `record_trust` logs
    /// that and keeps the in-memory decision either way.
    fn record_store_degraded(&mut self, id: EventId) {
        self.record_trust(TrustDecision {
            code: "store_degraded",
            severity: Severity::Critical,
            admin_seq: 0,
            event_ids: vec![id],
        });
    }

    /// Promote any parked frames the fold has since reclassified as accepted, and
    /// drop any it now rejects (cascades grandchildren in the outer loop).
    fn wake_park(&mut self, out: &mut Vec<Outgoing>) {
        loop {
            let mut changed = false;
            for id in self.park.keys().copied().collect::<Vec<_>>() {
                let ev = self.park[&id].event.clone();
                match self.fold.ingest(ev.clone()) {
                    crate::membership::Ingest::Accepted { .. } => {
                        self.park.remove(&id);
                        self.backfill_depth.remove(&id);
                        self.restored_backfill.remove(&id);
                        self.persist_delete_parked(id);
                        self.store_and_fanout(&ev, None, out);
                        changed = true;
                    }
                    crate::membership::Ingest::Rejected { reason, .. } => {
                        self.park.remove(&id);
                        self.backfill_depth.remove(&id);
                        self.restored_backfill.remove(&id);
                        self.persist_delete_parked(id);
                        self.counters.rejected += 1;
                        self.log(&format!("reject.{}", reason.code()));
                        changed = true;
                    }
                    crate::membership::Ingest::Buffered { .. } => {}
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// The §6.2 anti-amplification gate, then park + rate-limited backfill.
    fn on_buffered(
        &mut self,
        ev: ValidatedEvent,
        from: Option<PeerId>,
        missing: &[EventId],
        out: &mut Vec<Outgoing>,
    ) {
        let id = ev.event_id;
        let depth = self.backfill_depth.get(&id).copied().unwrap_or(0);

        // Gate 2/5: phantom-parent depth bound (structural proxy for an
        // implausible derived lamport). A real chain gap deeper than this is
        // dropped here and is permanently unrecoverable by this node through the
        // backfill path (spec §4.4), so surface it the way #119 surfaces
        // `store_degraded`: count every drop and record a CRITICAL
        // `backfill_depth_exceeded` decision. This is a hot path (one call per
        // buffered frame, floodable by fabricated deep chains), so the CRITICAL
        // record is latched to the *first* drop of the session — the counter
        // carries the ongoing volume — rather than storming the audit sink with a
        // per-event decision (its own denial of service).
        if depth > self.config.max_backfill_depth {
            if self.counters.phantom_depth_dropped == 0 {
                self.record_trust(TrustDecision {
                    code: "backfill_depth_exceeded",
                    severity: Severity::Critical,
                    admin_seq: 0,
                    event_ids: vec![id],
                });
            }
            self.counters.phantom_depth_dropped += 1;
            self.log(&format!(
                "reject.phantom_parent_depth id={id} author={} depth={depth}",
                ev.event.sender_id
            ));
            return;
        }

        // Gate 1: signer pre-check. A frame from a key that is not even plausibly
        // in the room never earns a park or a backfill fan-out (spec §13 vector).
        if !self.signer_plausible(&ev) {
            self.counters.signer_dropped += 1;
            self.log("reject.anti_amplification_signer");
            return;
        }

        // Gates 3: park within the per-author + total caps (oldest-first eviction).
        self.park_frame(ev, depth, missing);

        // Gate 4: rate-limited backfill of the still-missing parents.
        let author = self.park.get(&id).map(|p| p.author);
        let Some(author) = author else { return };
        let to_fetch: Vec<EventId> = missing
            .iter()
            .copied()
            .filter(|m| self.worth_backfilling(m))
            .collect();
        if to_fetch.is_empty() {
            return;
        }
        if !self.take_backfill_token(author) {
            self.counters.backfill_rate_limited += 1;
            self.log("backfill suppressed: backfill_rate_limited");
            return;
        }
        for m in &to_fetch {
            self.backfill_depth.entry(*m).or_insert(depth + 1);
        }
        // Direct the pull at the sender if known, else broadcast to peers.
        let targets: Vec<PeerId> = match from {
            Some(p) if self.peers.contains(&p) => vec![p],
            _ => self.peers.iter().copied().collect(),
        };
        for chunk in to_fetch.chunks(self.config.max_backfill_fanout_ids) {
            for &peer in &targets {
                out.push(Outgoing {
                    peer,
                    msg: SyncMessage::WantEvents {
                        room_id: self.room_id,
                        ids: chunk.to_vec(),
                    },
                });
                self.counters.backfill_requests += 1;
            }
        }
    }

    /// Whether `ev` would buffer in the fold for want of a parent — i.e. at least
    /// one cited `prev_event` is not yet in the local validated set. The store
    /// holds exactly the fold-accepted set (spec D5), so a parent is "present" iff
    /// the store contains it; a store-read error is treated conservatively as
    /// absent (would buffer). Used by the §6.2 pre-gate so non-member junk is kept
    /// out of the fold entirely (the fold has no eviction).
    fn would_buffer(&self, ev: &ValidatedEvent) -> bool {
        ev.event.prev_events.iter().any(|parent| {
            !self
                .store
                .contains_in_room(&self.room_id, parent)
                .unwrap_or(false)
        })
    }

    /// A buffered frame's signer is plausibly in the room iff it is the admin, a
    /// known subject, or a `member.joined` (the one event that legitimately
    /// arrives from a not-yet-known invitee; its capability is proven by
    /// backfilling the invite, bounded by depth). Chat/file/pipe junk from an
    /// unknown key is dropped (spec §6.2 step 1).
    fn signer_plausible(&self, ev: &ValidatedEvent) -> bool {
        if matches!(ev.event.content, Content::MemberJoined(_)) {
            return true;
        }
        let snap = self.fold.snapshot();
        let sender = &ev.event.sender_id;
        snap.admin() == Some(sender) || snap.member(sender).is_some()
    }

    /// Whether a still-missing parent is worth a `WantEvents` fetch. A parent is
    /// worth fetching only if it is **neither already stored** (nothing to fetch)
    /// **nor already parked** (its own retry is already chasing *its* parents, so
    /// re-requesting it makes no forward progress).
    ///
    /// Excluding already-parked parents is what un-sticks a deep single-author
    /// backfill (issue #114): when a whole windowed run parks, every frame but the
    /// deepest cites a parent that is itself parked, so without this filter the
    /// rate-limited retry burns its entire per-author token budget re-requesting
    /// frames it already holds — and the one request that would advance the gap
    /// frontier loses the token race deterministically, freezing the chase. With
    /// it, tokens are spent only on the true frontier (a parent that is neither
    /// held nor in flight), so the chase descends toward the held set.
    fn worth_backfilling(&self, parent: &EventId) -> bool {
        !self
            .store
            .contains_in_room(&self.room_id, parent)
            .unwrap_or(false)
            && !self.park.contains_key(parent)
    }

    fn park_frame(&mut self, ev: ValidatedEvent, depth: usize, missing: &[EventId]) {
        let id = ev.event_id;
        if self.park.contains_key(&id) {
            return;
        }
        let author = ev.event.sender_id;

        // Enforce the global cap (oldest-first).
        while self.park.len() >= self.config.max_parked_total {
            if let Some(evict) = self.oldest_parked(None) {
                self.evict_parked(evict, "reject.park_evicted: max_parked_total");
            } else {
                break;
            }
        }
        // Enforce the per-author cap (oldest of that author first).
        let author_count = self.park.values().filter(|p| p.author == author).count();
        if author_count >= self.config.max_parked_per_author {
            if let Some(evict) = self.oldest_parked(Some(author)) {
                self.evict_parked(evict, "reject.park_evicted: max_parked_per_author");
            }
        }

        self.park_seq += 1;
        let seq = self.park_seq;
        self.park.insert(
            id,
            Parked {
                event: ev,
                author,
                seq,
                depth,
                missing: missing.iter().copied().collect(),
            },
        );
        self.counters.parked += 1;
        // Checkpoint the new park row + its missing-parent edges (spec §6.2 / D4).
        self.persist_parked(id);
    }

    /// Evict one parked frame (cap overflow): drop it from memory and the
    /// persisted park, count and log it. A frame evicted here and not later
    /// re-served is permanently lost (spec §4.4), so — mirroring the #119
    /// `store_degraded` path — record a CRITICAL `park_overflow` decision naming
    /// the victim. Eviction is a hot, floodable path, so the CRITICAL record is
    /// latched to the *first* eviction of the session (the `park_evicted` counter
    /// carries the volume) rather than one decision per evicted frame.
    fn evict_parked(&mut self, id: EventId, reason: &str) {
        let author = self.park.get(&id).map(|p| p.author);
        self.park.remove(&id);
        self.backfill_depth.remove(&id);
        self.restored_backfill.remove(&id);
        self.persist_delete_parked(id);
        if self.counters.park_evicted == 0 {
            self.record_trust(TrustDecision {
                code: "park_overflow",
                severity: Severity::Critical,
                admin_seq: 0,
                event_ids: vec![id],
            });
        }
        self.counters.park_evicted += 1;
        match author {
            Some(a) => self.log(&format!("{reason} id={id} author={a}")),
            None => self.log(&format!("{reason} id={id}")),
        }
    }

    /// The oldest parked id (optionally restricted to one author), by arrival seq
    /// then id for a fully deterministic eviction choice.
    fn oldest_parked(&self, author: Option<IdentityKey>) -> Option<EventId> {
        self.park
            .iter()
            .filter(|(_, p)| author.map_or(true, |a| p.author == a))
            .min_by(|(id_a, a), (id_b, b)| a.seq.cmp(&b.seq).then(id_a.cmp(id_b)))
            .map(|(id, _)| *id)
    }

    /// Retry the park on a tick: re-emit backfill for still-missing parents,
    /// rate-limited (spec §6.3 — orphans are retried, never silently discarded).
    fn retry_park(&mut self, out: &mut Vec<Outgoing>) {
        // First promote anything the fold has since accepted.
        self.wake_park(out);
        // Use each frame's **recorded** missing-parent set, not a store lookup: a
        // parked frame is not in `events`, so it has no `event_parents` rows and
        // `missing_parents_in_room` would return empty — the tick-driven retry
        // would then never re-fetch for a still-parked chain, stalling a deep
        // backfill once the initial `on_buffered` token burst is spent (issue
        // #114). `worth_backfilling` drops the parents that have since been stored
        // or are already parked in flight.
        let pending: Vec<(IdentityKey, usize, Vec<EventId>)> = self
            .park
            .values()
            .map(|p| (p.author, p.depth, p.missing.iter().copied().collect()))
            .collect();
        for (author, depth, missing) in pending {
            let to_fetch: Vec<EventId> = missing
                .into_iter()
                .filter(|m| self.worth_backfilling(m))
                .collect();
            if to_fetch.is_empty() {
                continue;
            }
            if !self.take_backfill_token(author) {
                self.counters.backfill_rate_limited += 1;
                continue;
            }
            for m in &to_fetch {
                self.backfill_depth.entry(*m).or_insert(depth + 1);
            }
            for chunk in to_fetch.chunks(self.config.max_backfill_fanout_ids) {
                for peer in self.peers.iter().copied().collect::<Vec<_>>() {
                    out.push(Outgoing {
                        peer,
                        msg: SyncMessage::WantEvents {
                            room_id: self.room_id,
                            ids: chunk.to_vec(),
                        },
                    });
                    self.counters.backfill_requests += 1;
                }
            }
        }
    }

    fn take_backfill_token(&mut self, author: IdentityKey) -> bool {
        let bucket = self
            .tokens
            .entry(author)
            .or_insert(self.config.backfill_tokens_per_author);
        if *bucket == 0 {
            false
        } else {
            *bucket -= 1;
            // Checkpoint the depleted bucket so a crash-loop cannot reset the
            // amplification budget (spec §1.3 / §6.3 / R4).
            self.persist_tokens();
            true
        }
    }

    fn refill_tokens(&mut self) {
        let cap = self.config.backfill_tokens_per_author;
        let refill = self.config.backfill_refill_per_tick;
        let mut changed = false;
        for bucket in self.tokens.values_mut() {
            let next = bucket.saturating_add(refill).min(cap);
            if next != *bucket {
                *bucket = next;
                changed = true;
            }
        }
        // One batched checkpoint per tick (spec §6.2 token-refill row).
        if changed {
            self.persist_tokens();
        }
    }

    // ------------------------------------------------------------------
    // Serving pulls (§6.4)
    // ------------------------------------------------------------------

    fn serve_want_events(&mut self, from: PeerId, ids: &[EventId], out: &mut Vec<Outgoing>) {
        let mut frames = Vec::new();
        let mut missing = Vec::new();
        for id in ids {
            if frames.len() >= self.config.response_max_frames {
                self.log("Events response capped: response_max_frames");
                break;
            }
            match self.store.get_in_room(&self.room_id, id) {
                Ok(Some(se)) => frames.push(se.wire.to_bytes()),
                Ok(None) => missing.push(*id),
                Err(e) => self.log(&format!("store get failed: {e}")),
            }
        }
        self.emit_events(from, frames, out);
        if !missing.is_empty() {
            out.push(to(
                from,
                SyncMessage::NotFound {
                    room_id: self.room_id,
                    ids: missing,
                },
            ));
        }
    }

    /// Serve the never-windowed authorization-class set **causally closed** —
    /// the §4.1 class plus every stored `prev_events` ancestor — minus what the
    /// requester's `have` **ancestry claims** cover, in causal order (spec §6.4 —
    /// the §0 hard invariant). The closure is what makes the invariant real: a
    /// membership event authored after chat cites chat heads as structural
    /// parents, and the fold cannot classify it until *every* parent is present,
    /// so serving the bare class would hand a bootstrapping joiner an
    /// unverifiable sub-DAG (the join-after-history deadlock). Ancestors ride
    /// along even when chat-class: membership verifiability is never bounded by
    /// the chat window.
    ///
    /// Each claimed id covers itself plus every stored ancestor
    /// ([`covered_by_claim`](Self::covered_by_claim), issue #113), so a bounded
    /// claim subtracts an arbitrarily large held set — a pre-#113 exhaustive
    /// claim over an intact store (itself causally closed) expands to exactly
    /// itself and is served identically (see the `WantMembership` doc for the
    /// old-requester store-hole exception).
    fn serve_want_membership(&mut self, from: PeerId, have: &[EventId], out: &mut Vec<Outgoing>) {
        let ids = match self.authorization_closure_ids() {
            Ok(s) => s,
            Err(e) => {
                self.log(&format!("authorization-class scan failed: {e}"));
                return;
            }
        };
        let covered = match self.covered_by_claim(have) {
            Ok(Coverage::Everything) => return, // converged: nothing to serve
            Ok(Coverage::Set(covered)) => covered,
            Err(e) => {
                self.log(&format!("have-claim expansion failed: {e}"));
                return;
            }
        };
        let mut stored = Vec::new();
        for id in ids.difference(&covered) {
            match self.store.get_in_room(&self.room_id, id) {
                // A NULL-lamport row sits above a local store hole (an ancestor
                // was never persisted): its ancestry cannot be served complete,
                // and `None` sorts before genesis in `causal_order`, so it would
                // corrupt the causally-closed prefix a truncated response must
                // be. Skip it — the row re-qualifies once the hole heals and the
                // store's insert-time propagation recomputes its lamport.
                Ok(Some(se)) if se.lamport.is_some() => stored.push(se),
                Ok(Some(_)) => self.log("WantMembership: skipped causally-incomplete row"),
                Ok(None) => {}
                Err(e) => self.log(&format!("store get failed: {e}")),
            }
        }
        stored.sort_by(causal_order);
        let frames = self.frames_from_stored(&stored, "WantMembership");
        self.emit_events(from, frames, out);
    }

    /// Expand a `WantMembership` `have` **ancestry claim** (issue #113) into the
    /// ids it covers: every claimed id held in this room, plus every stored
    /// ancestor of one.
    ///
    /// Soundness (no under-serve): an honest requester claims only ids whose
    /// entire ancestry it holds (non-NULL lamport — see
    /// [`membership_have`](Self::membership_have)), and ids are content hashes,
    /// so the ancestry expanded here is byte-identical to what the requester
    /// holds: covered ⊆ requester-held, always. A claimed id we do not hold is
    /// skipped, and skipping only **under**-covers — the responder then
    /// over-serves idempotent duplicates, the safe direction — so a stale,
    /// foreign, or malicious claim can waste bandwidth but never withhold an
    /// event (§0 stays intact; a false claim only starves the claimant itself).
    ///
    /// Fast path: a claim containing every local DAG head covers the entire held
    /// set (every held event is an ancestor-or-self of some head), so the
    /// converged mesh answers without any walk. Otherwise the downset BFS is
    /// `O(held)` `parents_of` lookups, memoized per exact claimed-and-held
    /// frontier in `covered_cache` (invalidated on insert) so a peer repeating
    /// the same claim across ticks pays it once.
    fn covered_by_claim(&mut self, have: &[EventId]) -> Result<Coverage, StoreError> {
        let mut frontier = BTreeSet::new();
        for id in have {
            if self.store.contains_in_room(&self.room_id, id)? {
                frontier.insert(*id);
            }
        }
        if frontier.is_empty() {
            return Ok(Coverage::Set(BTreeSet::new()));
        }
        let heads = self.store.heads(&self.room_id)?;
        if !heads.is_empty() && heads.iter().all(|h| frontier.contains(h)) {
            return Ok(Coverage::Everything);
        }
        if let Some(covered) = self.covered_cache.get(&frontier) {
            return Ok(Coverage::Set(covered.clone()));
        }
        let mut covered = frontier.clone();
        let mut stack: Vec<EventId> = frontier.iter().copied().collect();
        while let Some(id) = stack.pop() {
            // Every id expanded here descends from a held same-room event (the
            // frontier is gated on `contains_in_room`, and a stored event's
            // parent edges are same-room ancestry — the id content-hashes the
            // room), so the walk is bounded by the held set. A dangling parent
            // edge (a local hole) ends its branch: the absent id has no stored
            // edges of its own, and subtracting a non-stored id from the closure
            // is a no-op, so no per-parent `contains` re-check is needed.
            for parent in self.store.parents_of(&id)? {
                if covered.insert(parent) {
                    stack.push(parent);
                }
            }
        }
        if self.covered_cache.len() >= COVERED_CACHE_ENTRIES {
            // Deterministic eviction (spec R4): drop the smallest key.
            self.covered_cache.pop_first();
        }
        self.covered_cache.insert(frontier, covered.clone());
        Ok(Coverage::Set(covered))
    }

    fn serve_want_recent_chat(
        &mut self,
        from: PeerId,
        window: Window,
        have: &[EventId],
        out: &mut Vec<Outgoing>,
    ) {
        let have: BTreeSet<EventId> = id_set(have.iter().copied());
        let limit = self.config.effective_window(window.max_count);
        // Fetch up to the hard cap, keep chat-class only, then take the last
        // `limit` in canonical order (the trustworthy bound).
        let tail = match self
            .store
            .room_tail(&self.room_id, self.config.chat_window_max)
        {
            Ok(t) => t,
            Err(e) => {
                self.log(&format!("room_tail failed: {e}"));
                return;
            }
        };
        let mut chat: Vec<_> = tail.into_iter().filter(is_chat_class).collect();
        // `room_tail` is ascending; the last `limit` are the most recent.
        let start = chat.len().saturating_sub(limit as usize);
        chat.drain(..start);
        let mut selected: Vec<_> = chat
            .into_iter()
            .filter(|se| !have.contains(&se.event_id))
            .filter(|se| advisory_since(se, window.since_ms))
            .collect();
        selected.sort_by(causal_order);
        let frames = self.frames_from_stored(&selected, "WantRecentChat");
        self.emit_events(from, frames, out);
    }

    /// The authorization-class id set (spec §4.1): the five membership types plus
    /// every admin-authored event, regardless of any chat window.
    fn authorization_class_ids(&self) -> Result<BTreeSet<EventId>, StoreError> {
        let mut ids = BTreeSet::new();
        for ty in MEMBERSHIP_TYPES {
            for se in self.store.by_type(&self.room_id, ty)? {
                ids.insert(se.event_id);
            }
        }
        if let Some(admin) = self.fold.snapshot().admin() {
            for se in self.store.by_sender(&self.room_id, admin)? {
                ids.insert(se.event_id);
            }
        }
        Ok(ids)
    }

    /// The causal closure of the authorization class: the §4.1 class plus every
    /// stored ancestor reachable over `prev_events`. This — not the bare class —
    /// is what `WantMembership` serves: the fold's readiness rule needs the
    /// *complete* structural ancestry of each membership event, and once a
    /// conversation has happened that ancestry includes non-admin chat. The
    /// store holds only fold-accepted events (whose parents were present at
    /// accept time), so the walk terminates at genesis; a dangling edge is
    /// skipped, never fabricated.
    /// Memoized in `closure_cache` (invalidated on insert) so a quiesced mesh —
    /// or a `WantMembership`-spamming provisional dialer — pays the walk once,
    /// not per request.
    fn authorization_closure_ids(&mut self) -> Result<BTreeSet<EventId>, StoreError> {
        if let Some(cache) = &self.closure_cache {
            return Ok(cache.clone());
        }
        let mut ids = self.authorization_class_ids()?;
        let mut frontier: Vec<EventId> = ids.iter().copied().collect();
        while let Some(id) = frontier.pop() {
            // Every id in the walk is a same-room stored event (the class comes
            // from room-scoped queries and the room-scoped contains below gates
            // each addition), so its `event_parents` edges are same-room ancestry.
            for parent in self.store.parents_of(&id)? {
                if !ids.contains(&parent) && self.store.contains_in_room(&self.room_id, &parent)? {
                    ids.insert(parent);
                    frontier.push(parent);
                }
            }
        }
        self.closure_cache = Some(ids.clone());
        Ok(ids)
    }

    fn frames_from_stored(
        &mut self,
        stored: &[crate::store::StoredEvent],
        label: &str,
    ) -> Vec<Vec<u8>> {
        let mut frames: Vec<Vec<u8>> = stored.iter().map(|se| se.wire.to_bytes()).collect();
        if frames.len() > self.config.response_max_frames {
            self.log(&format!(
                "{label} response capped at response_max_frames ({} dropped)",
                frames.len() - self.config.response_max_frames
            ));
            frames.truncate(self.config.response_max_frames);
        }
        frames
    }

    /// Emit `frames` as one or more [`Events`](SyncMessage::Events) messages,
    /// each staying under the [`MAX_FRAME_BYTES`](super::message::MAX_FRAME_BYTES)
    /// wire cap (issue #113): a single unchunked batch of `response_max_frames`
    /// frames can encode to several MiB (frames run up to ~17 KiB), and an
    /// oversized body is dropped at the net writer — the responder would then
    /// re-serve the identical batch every tick, a permanent stall. Chunks are
    /// consecutive, so causal order is preserved across the split; the
    /// `response_max_frames` cap was applied upstream, so total frames per serve
    /// — the anti-amplification quantity — are unchanged.
    fn emit_events(&mut self, from: PeerId, frames: Vec<Vec<u8>>, out: &mut Vec<Outgoing>) {
        if frames.is_empty() {
            return;
        }
        self.counters.frames_sent += frames.len() as u64;
        let budget = super::message::MAX_FRAME_BYTES - EVENTS_ENVELOPE_ALLOWANCE;
        let mut batch: Vec<Vec<u8>> = Vec::new();
        let mut batch_cost = 0usize;
        for frame in frames {
            let cost = frame.len() + EVENTS_PER_FRAME_OVERHEAD;
            if !batch.is_empty() && batch_cost + cost > budget {
                out.push(to(
                    from,
                    SyncMessage::Events {
                        room_id: self.room_id,
                        frames: std::mem::take(&mut batch),
                    },
                ));
                batch_cost = 0;
            }
            if batch.is_empty() && cost > budget {
                // A frame cannot be split. Anything that arrived over the wire
                // re-sends alone within the cap (a single-frame envelope costs
                // less than this allowance); only a pre-guard oversized local
                // event can still exceed it, and that is the writer's logged
                // drop — same as before, now visible here too.
                self.log("Events frame exceeds wire budget; sent alone");
            }
            batch_cost += cost;
            batch.push(frame);
        }
        if !batch.is_empty() {
            out.push(to(
                from,
                SyncMessage::Events {
                    room_id: self.room_id,
                    frames: batch,
                },
            ));
        }
    }

    // ------------------------------------------------------------------
    // Handshake handlers (§6.3) + admin-tip detector (§D6)
    // ------------------------------------------------------------------

    fn handle_admin_tip(&mut self, tip: Option<(EventId, u64)>, out: &mut Vec<Outgoing>) {
        if let Some((id, seq)) = tip {
            // An `AdminTip` is an **unverified** peer claim, not proof the event
            // exists. Do NOT feed it into the held admin state (`admin_ids_by_seq`
            // / the local tip): doing so would let a single peer forge a fork
            // against the honest admin (a fabricated id colliding at a seq we hold)
            // or, via a bogus huge seq, pin us fail-closed forever on a tip that
            // can never be backfilled. Treat it only as a *suspect* tip that drives
            // a bounded catch-up pull (spec D6 / §13).
            let local = self.store.admin_chain_tip(&self.room_id).ok().flatten();
            let behind = local.map_or(true, |(_, loc)| seq > loc)
                && !self
                    .store
                    .contains_in_room(&self.room_id, &id)
                    .unwrap_or(false);
            if behind {
                self.arm_suspect_tip(id, seq);
                // Pull the never-windowed membership sub-DAG from every connected
                // peer to close the gap (the §0 hard invariant: membership is never
                // windowed). If the tip is real it backfills and clears the
                // suspicion; if fabricated it is expired by the attempt budget.
                let have = self.membership_have();
                for peer in self.peers.iter().copied().collect::<Vec<_>>() {
                    out.push(Outgoing {
                        peer,
                        msg: SyncMessage::WantMembership {
                            room_id: self.room_id,
                            have: have.clone(),
                        },
                    });
                }
            }
        }
        if let Err(e) = self.recompute_completeness() {
            self.log(&format!("completeness recompute failed: {e}"));
        }
    }

    /// Arm (or re-arm) the unconfirmed suspect tip. A genuinely new advertisement
    /// (different id or higher seq) resets the attempt budget; a repeat of the tip
    /// we are already chasing does not, so a peer re-advertising the same
    /// fabricated tip every tick cannot indefinitely refresh the budget (spec §13).
    fn arm_suspect_tip(&mut self, id: EventId, seq: u64) {
        let fresh = match self.suspect_tip {
            Some(t) => t.id != id || t.seq != seq,
            None => true,
        };
        if fresh {
            self.suspect_tip = Some(SuspectTip {
                id,
                seq,
                attempts: self.config.max_unconfirmed_tip_attempts,
            });
            // Persist the raised suspicion so a restart re-arms the fail-closed
            // gate before any access decision is served (spec §1.1 / D3).
            self.persist_sync_state();
        }
    }

    /// Decrement the unconfirmed suspect tip's attempt budget on each tick and drop
    /// it when exhausted, so a fabricated higher tip cannot pin the node fail-closed
    /// forever (spec D6 / §13 — advertisements are hints, never proof). A *real*
    /// tip is cleared earlier by [`recompute_completeness`](Self::recompute_completeness)
    /// the moment its event is backfilled and stored, so in practice only a
    /// never-backfillable (fabricated, or no-longer-reachable) tip reaches expiry.
    fn expire_suspect_tip(&mut self) {
        let Some(susp) = self.suspect_tip else {
            return;
        };
        if susp.attempts == 0 {
            self.suspect_tip = None;
            self.persist_sync_state();
            // Expiring the suspicion FAILS OPEN the removal-sensitive access gate:
            // the node stops failing closed on a tip it could never confirm, so a
            // removal it never saw could leave a removed member trusted as active.
            // Unlike the advisory admin-view-suspect warning, this transition
            // weakens a security property, so record it as CRITICAL
            // `admin_tip_expired` naming the abandoned tip. Expiry is tick-rate
            // bounded and needs a fresh fabricated tip each time (not a hot path),
            // but to stay consistent with the other fix-2 bounds — and to blunt a
            // rotating-tip attacker — the CRITICAL record is latched to the first
            // expiry of the session while the counter tracks every one.
            if self.counters.suspect_tip_expired == 0 {
                self.record_trust(TrustDecision {
                    code: "admin_tip_expired",
                    severity: Severity::Critical,
                    admin_seq: susp.seq,
                    event_ids: vec![susp.id],
                });
            }
            self.counters.suspect_tip_expired += 1;
            self.log("unconfirmed admin tip expired: admin_tip_unconfirmed");
            if let Err(e) = self.recompute_completeness() {
                self.log(&format!("completeness recompute failed: {e}"));
            }
        } else {
            self.suspect_tip = Some(SuspectTip {
                attempts: susp.attempts - 1,
                ..susp
            });
            // Persist the decremented budget so the bound spans a restart (R4).
            self.persist_sync_state();
        }
    }

    /// Handle a peer's advertised DAG heads.
    ///
    /// Heads are an **advisory** delta hint only (spec OQ-2): the engine does
    /// **not** chase unknown heads by id. Doing so would pull chat events that the
    /// requester deliberately left outside its bounded window (every chat head
    /// would be fetched), defeating the §10.7 count bound. The correctness paths
    /// are the never-windowed `WantMembership` pull, the bounded `WantRecentChat`
    /// pull, and the by-id backfill of a *buffered* frame's missing parents (§6.2)
    /// — none of which a raw head advertisement can bypass.
    fn handle_heads(&mut self, heads: &[EventId]) {
        let unknown = heads
            .iter()
            .filter(|id| {
                !self
                    .store
                    .contains_in_room(&self.room_id, id)
                    .unwrap_or(false)
            })
            .count();
        if unknown > 0 {
            self.log(&format!(
                "noted {unknown} unknown advertised heads (advisory)"
            ));
        }
    }

    /// Re-derive the completeness verdict, fail-closed subject set, and trust
    /// decisions from current admin state (spec D6).
    fn recompute_completeness(&mut self) -> Result<(), StoreError> {
        // Fork: an `admin_seq` at which we hold **two distinct, validated** admin
        // events (spec §7). Every id in `admin_ids_by_seq` came from an
        // accepted-and-stored admin event (never from an advertisement), so a peer
        // cannot forge a fork by advertising a fake tip; we re-confirm the
        // conflicting ids are still held so the alarm always names branches this
        // node truly holds. A real cross-partition fork is still detected once the
        // never-windowed membership pull backfills the other branch.
        let mut fork = None;
        for (seq, ids) in &self.admin_ids_by_seq {
            if ids.len() < 2 {
                continue;
            }
            let mut held = Vec::new();
            for id in ids {
                if self.store.contains_in_room(&self.room_id, id)? {
                    held.push(*id);
                }
            }
            if held.len() >= 2 {
                fork = Some((*seq, held));
                break;
            }
        }

        // Behind: an advertised (unverified) tip ahead of our local held chain that
        // we have not yet backfilled. Cleared here the moment we hold it / our local
        // tip catches up; otherwise expired by the bounded attempt budget on tick
        // (spec §13), so a fabricated tip cannot pin us fail-closed forever.
        let local = self.store.admin_chain_tip(&self.room_id)?;
        let mut suspicion_cleared = false;
        let behind = if let Some(susp) = self.suspect_tip {
            let still_behind = local.map_or(true, |(_, loc)| susp.seq > loc)
                && !self.store.contains_in_room(&self.room_id, &susp.id)?;
            if !still_behind {
                self.suspect_tip = None;
                suspicion_cleared = true;
            }
            still_behind
        } else {
            false
        };

        self.completeness = if fork.is_some() {
            Completeness::AdminForkDetected
        } else if behind {
            Completeness::AdminViewSuspect
        } else {
            Completeness::Complete
        };

        if let Some((seq, ids)) = fork {
            self.record_trust(TrustDecision {
                code: "equivocation",
                severity: Severity::Critical,
                admin_seq: seq,
                event_ids: ids,
            });
        } else if behind {
            if let Some(susp) = self.suspect_tip {
                self.record_trust(TrustDecision {
                    code: "admin_view_suspect",
                    severity: Severity::Warning,
                    admin_seq: susp.seq,
                    event_ids: vec![susp.id],
                });
            }
        }

        // Fail closed on every removal-sensitive subject while incomplete: any
        // not-yet-applied admin event could remove an active member, and we
        // cannot know which, so we deny on all non-admin, non-removed subjects
        // (the conservative, safe fail-closed set; spec D6 / §10).
        self.fail_closed.clear();
        if self.completeness != Completeness::Complete {
            let snap = self.fold.snapshot();
            let admin = snap.admin().copied();
            for member in snap.members() {
                if Some(member.identity) != admin && member.status != Status::Removed {
                    self.fail_closed.insert(member.identity);
                }
            }
        }
        // Persist a catch-up that cleared the suspicion, so the resolved posture
        // survives a subsequent restart (spec §6.2 suspicion-clear row).
        if suspicion_cleared {
            self.persist_sync_state();
        }
        Ok(())
    }

    fn record_trust(&mut self, decision: TrustDecision) {
        if !self.trust_decisions.contains(&decision) {
            // Append to the durable audit trail first (a reboot must not erase a
            // CRITICAL admin-fork alert, spec D6), then hold it in memory.
            self.persist_trust(&decision);
            self.trust_decisions.push(decision);
        }
    }

    // ------------------------------------------------------------------
    // Restart durability (IR-0201 §6.1–§6.3): restore transient state on
    // `open` and checkpoint each mutation. `events` stays authoritative; the
    // five v2 tables are droppable derived caches. A checkpoint fault is a
    // logged, **non-fatal** degradation (durability, never correctness — a
    // reconnect re-pulls; spec §6.2 / §9), so persist helpers log and continue.
    // ------------------------------------------------------------------

    /// Restore the persisted park, unconfirmed suspicion, backfill token buckets,
    /// and trust-decision audit on `open` (spec §6.1). The held fold / admin chain
    /// / derived completeness are **rebuilt from `events`**, never persisted (D1).
    fn restore_persisted_state(&mut self) -> Result<(), StoreError> {
        // --- park: re-validate every stored `wire`; drop+log corrupt rows (D5) ---
        let parked = self.store.load_parked(&self.room_id)?;
        let ctx = ValidationContext::for_room(self.room_id);
        let mut max_seq = self.park_seq;
        for row in parked {
            match validate_wire_bytes(&row.wire, &ctx) {
                Ok(ev) => {
                    max_seq = max_seq.max(row.park_seq);
                    let depth = usize::try_from(row.depth).unwrap_or(usize::MAX);
                    self.park.insert(
                        row.event_id,
                        Parked {
                            event: ev,
                            author: row.author,
                            seq: row.park_seq,
                            depth,
                            missing: row.missing.into_iter().collect(),
                        },
                    );
                    self.restored_backfill.insert(row.event_id);
                    self.counters.parked_restored += 1;
                }
                Err(reason) => {
                    // A corrupt/tampered park row can never be trusted: drop it and
                    // log a stable `reject.park_corrupt`, never a panic (spec §9/R3).
                    let _ = self.store.delete_parked(&self.room_id, &row.event_id);
                    self.counters.park_corrupt_dropped += 1;
                    self.log(&format!("reject.park_corrupt: {}", reason.code()));
                }
            }
        }
        self.park_seq = max_seq;

        // --- sync_state: the unconfirmed admin-tip suspicion (anti fail-open, D3);
        //     the chat cursor is advisory and intentionally not consumed (OQ-1) ---
        if let Some(st) = self.store.load_sync_state(&self.room_id)? {
            if let Some((id, seq, attempts)) = st.suspect_tip {
                self.suspect_tip = Some(SuspectTip { id, seq, attempts });
                self.counters.suspicion_restored += 1;
            }
        }

        // --- backfill token buckets: restored as-is, NOT refilled by the restart
        //     (the amplification budget must not reset; spec §1.3 / §6.3) ---
        let tokens = self.store.load_backfill_tokens(&self.room_id)?;
        self.counters.tokens_restored = tokens.len() as u64;
        self.tokens = tokens;

        // --- trust-decision audit: loaded into the live list (not re-persisted),
        //     so the trail grows across restarts and stays queryable (D6) ---
        for tr in self.store.load_trust_decisions(&self.room_id)? {
            let decision = trust_row_to_decision(&tr)?;
            self.trust_decisions.push(decision);
            self.counters.trust_restored += 1;
        }
        Ok(())
    }

    /// Re-issue by-id `WantEvents` backfill for every restored parked frame, once,
    /// on the first `on_connect`/`on_tick` after `open` (spec §6.3). Gated by the
    /// **restored** token buckets (not a fresh budget), so buffering *and retry*
    /// survive a restart without resetting the amplification bound. Thereafter the
    /// frame retries like any other (anti-entropy pulls + [`wake_park`](Self::wake_park)).
    fn retry_restored_park(&mut self, out: &mut Vec<Outgoing>) {
        if self.restored_backfill.is_empty() || self.peers.is_empty() {
            return;
        }
        for id in self.restored_backfill.iter().copied().collect::<Vec<_>>() {
            // One-shot: clear regardless of outcome; ongoing retry is anti-entropy.
            self.restored_backfill.remove(&id);
            let Some((author, depth, missing)) = self
                .park
                .get(&id)
                .map(|p| (p.author, p.depth, p.missing.clone()))
            else {
                continue;
            };
            let to_fetch: Vec<EventId> = missing
                .into_iter()
                .filter(|m| self.worth_backfilling(m))
                .collect();
            if to_fetch.is_empty() {
                continue;
            }
            if !self.take_backfill_token(author) {
                self.counters.backfill_rate_limited += 1;
                self.log("backfill suppressed: backfill_rate_limited");
                continue;
            }
            for m in &to_fetch {
                self.backfill_depth.entry(*m).or_insert(depth + 1);
            }
            for chunk in to_fetch.chunks(self.config.max_backfill_fanout_ids) {
                for peer in self.peers.iter().copied().collect::<Vec<_>>() {
                    out.push(Outgoing {
                        peer,
                        msg: SyncMessage::WantEvents {
                            room_id: self.room_id,
                            ids: chunk.to_vec(),
                        },
                    });
                    self.counters.backfill_requests += 1;
                }
            }
        }
    }

    /// Checkpoint one parked frame (row + its missing-parent edges).
    fn persist_parked(&mut self, id: EventId) {
        let Some(row) = self.park.get(&id).map(|p| ParkedRow {
            event_id: id,
            wire: p.event.wire.to_bytes(),
            author: p.author,
            park_seq: p.seq,
            depth: u32::try_from(p.depth).unwrap_or(u32::MAX),
            missing: p.missing.iter().copied().collect(),
        }) else {
            return;
        };
        if let Err(e) = self.store.upsert_parked(&self.room_id, &row) {
            self.log(&format!("checkpoint failed: parked: {e}"));
        }
    }

    /// Checkpoint the removal of one parked frame.
    fn persist_delete_parked(&mut self, id: EventId) {
        if let Err(e) = self.store.delete_parked(&self.room_id, &id) {
            self.log(&format!("checkpoint failed: delete_parked: {e}"));
        }
    }

    /// Checkpoint the per-room `sync_state` row (the unconfirmed suspicion).
    fn persist_sync_state(&mut self) {
        let row = SyncStateRow {
            chat_cursor: None,
            suspect_tip: self.suspect_tip.map(|s| (s.id, s.seq, s.attempts)),
        };
        if let Err(e) = self.store.save_sync_state(&self.room_id, &row) {
            self.log(&format!("checkpoint failed: sync_state: {e}"));
        }
    }

    /// Checkpoint the per-author backfill token buckets.
    fn persist_tokens(&mut self) {
        if let Err(e) = self.store.save_backfill_tokens(&self.room_id, &self.tokens) {
            self.log(&format!("checkpoint failed: backfill_tokens: {e}"));
        }
    }

    /// Append one trust decision to the durable audit trail.
    fn persist_trust(&mut self, decision: &TrustDecision) {
        let row = TrustRow {
            seq: 0, // assigned by the store on append
            code: decision.code.to_owned(),
            severity: match decision.severity {
                Severity::Critical => "critical",
                Severity::Warning => "warning",
            }
            .to_owned(),
            admin_seq: Some(decision.admin_seq),
            event_ids: decision.event_ids.clone(),
            created_at: 0, // advisory only (the engine reads no wall clock)
        };
        if let Err(e) = self.store.append_trust_decision(&self.room_id, &row) {
            self.log(&format!("checkpoint failed: trust_decision: {e}"));
        }
    }

    /// Record a locally-accepted, **stored** admin event's `(admin_seq, id)` for
    /// fork detection. Only held-and-validated events feed this state (spec §7), so
    /// a peer cannot forge a fork by advertising a fabricated tip.
    fn note_admin_event(&mut self, id: EventId) {
        if let Ok(Some(se)) = self.store.get_in_room(&self.room_id, &id) {
            if let Some(seq) = se.admin_seq {
                self.note_admin_id(seq, id);
            }
        }
    }

    fn note_admin_id(&mut self, seq: u64, id: EventId) {
        self.admin_ids_by_seq.entry(seq).or_default().insert(id);
    }

    /// On open, seed the per-seq fork-detection state from the persisted, validated
    /// admin chain only (spec §9 restart determinism). Advertised tips are not yet
    /// known here and are never persisted, so nothing unverified is seeded.
    fn seed_admin_state(&mut self) -> Result<(), StoreError> {
        if let Some(admin) = self.fold.snapshot().admin() {
            for se in self.store.by_sender(&self.room_id, admin)? {
                if let Some(seq) = se.admin_seq {
                    self.note_admin_id(seq, se.event_id);
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Small helpers
    // ------------------------------------------------------------------

    fn admin_tip_msg(&self) -> SyncMessage {
        SyncMessage::AdminTip {
            room_id: self.room_id,
            tip: self.store.admin_chain_tip(&self.room_id).ok().flatten(),
        }
    }

    fn heads_msg(&self) -> SyncMessage {
        SyncMessage::Heads {
            room_id: self.room_id,
            heads: self.store.heads(&self.room_id).unwrap_or_default(),
        }
    }

    /// The `have` **ancestry claim** for a `WantMembership` pull (issue #113):
    /// a `membership_have_max_ids`-bounded sample of the held set — placed DAG
    /// heads (≤ half the budget), the most recent causally-placed ids, and a
    /// per-tick rotating window over everything older. Every claimed id has a
    /// non-`NULL` lamport, which the store derives only when the event's entire
    /// ancestry is present — so each id soundly claims itself **and all its
    /// stored ancestors** at the responder, and a bounded claim covers an
    /// arbitrarily large held set: unlike the pre-#113 exhaustive claim (every
    /// held id, ~34 B each), the frame no longer grows with room history and
    /// stays far below the 1 MiB wire cap.
    ///
    /// The heads carry the per-round progress invariant (#111): a truncated
    /// closure response is a causally-closed prefix that fold-accepts in full,
    /// its tips become heads, and — being responder-served — they are
    /// responder-known, so the next round's claim covers everything already
    /// delivered and the delta shrinks by the cap each round
    /// (`ceil(closure/cap)`-round bootstrap, quiescent when converged). The
    /// recent-lamport slab anchors the common catch-up shapes when heads alone
    /// are responder-unknown (a returning member whose newest events never
    /// fanned out would otherwise anchor nothing and be re-served from genesis
    /// every tick). The rotating window is the backstop for everything deeper:
    /// when even the slab is responder-unknown (an exclusive suffix larger than
    /// the whole budget), the sweep still claims every placed id within at most
    /// `placed` ticks, so the responder eventually anchors shared history and
    /// the pull cannot stay pinned at covered = ∅. Until the sweep lands, the
    /// responder re-serves duplicates (bounded per tick by
    /// `response_max_frames` and the byte budget); an adversarially **wide**
    /// shared region can stretch that window — full immunity is the deferred
    /// Meyer range reconciliation (a spec non-goal, tracked under #102).
    ///
    /// `NULL`-lamport rows (descendants stored above a local hole, e.g. after a
    /// swallowed insert error) are never claimed, so a responder keeps
    /// re-serving the missing ancestry until the hole heals — the same
    /// self-repair the exhaustive claim provided by omitting the hole itself.
    fn membership_have(&self) -> Vec<EventId> {
        let cap = self.config.membership_have_max_ids;
        let mut claim: BTreeSet<EventId> = BTreeSet::new();

        // 1) Placed DAG heads — the progress-invariant backbone — bounded to
        //    half the budget so a pathologically wide DAG (e.g. a member
        //    flooding parallel genesis-cited junk, every one a permanent head)
        //    cannot starve the anchors below.
        let head_budget = cap.div_ceil(2);
        for id in self.store.placed_heads(&self.room_id).unwrap_or_default() {
            if claim.len() >= head_budget {
                break;
            }
            claim.insert(id);
        }

        // 2) The recent-lamport slab — anchors the common catch-up shapes with
        //    the newest shared history.
        let slab_budget = cap.saturating_sub(claim.len()).div_ceil(2);
        let fetch = u32::try_from(slab_budget).unwrap_or(u32::MAX);
        for id in self
            .store
            .recent_event_ids(&self.room_id, fetch, 0)
            .unwrap_or_default()
        {
            if claim.len() >= cap {
                break;
            }
            claim.insert(id);
        }

        // 3) A rotating window over everything below the slab. Advancing by
        //    `window` per tick, it claims every placed id within at most
        //    `placed` ticks — and in a stalled state the store (hence the sweep
        //    span) is static, so the sweep is exact: even an exclusive suffix
        //    deeper than the whole budget cannot pin covered = ∅ forever.
        let window = cap.saturating_sub(claim.len());
        if window > 0 {
            let placed = self.store.placed_count(&self.room_id).unwrap_or(0);
            let span = placed.saturating_sub(slab_budget as u64);
            if span > 0 {
                let offset =
                    slab_budget as u64 + (self.claim_rotation.wrapping_mul(window as u64)) % span;
                let fetch = u32::try_from(window).unwrap_or(u32::MAX);
                for id in self
                    .store
                    .recent_event_ids(&self.room_id, fetch, offset)
                    .unwrap_or_default()
                {
                    claim.insert(id);
                }
            }
        }
        claim.into_iter().collect()
    }

    fn chat_have(&self) -> Vec<EventId> {
        self.store
            .room_tail(&self.room_id, self.config.chat_window_max)
            .map(|tail| {
                tail.into_iter()
                    .filter(is_chat_class)
                    .map(|se| se.event_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn log(&mut self, msg: &str) {
        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.remove(0);
        }
        self.logs.push(msg.to_owned());
    }
}

/// Build an [`Outgoing`] addressed to `peer`.
fn to(peer: PeerId, msg: SyncMessage) -> Outgoing {
    Outgoing { peer, msg }
}

/// Cheap pre-validation parse for the early event-id dedup path (issue #143 /
/// #134 §22.2, spec D1). Decodes only the outer [`WireEvent`] and recomputes
/// the id from `wire.signed` — never trusting the advisory `wire.id` field —
/// so the dedup cache can be consulted *before* signature verification or any
/// store work. A malformed envelope or an id mismatch surfaces as the same
/// [`RejectReason`] the full validator would return, preserving the cheap
/// `id_mismatch` / `non_canonical_encoding` reject codes that gate the cache
/// lookup.
///
/// On `Ok(id)`, the caller still MUST run [`validate_wire_bytes`]: this helper
/// intentionally performs no signature, canonical-CSB, content, or room-binding
/// check. Its only job is to derive a trustworthy id cheaply.
fn prevalidate_event_id(bytes: &[u8]) -> Result<EventId, RejectReason> {
    let wire = WireEvent::decode(bytes)?;
    let event_id = signed::event_id_from_bytes(&wire.signed);
    if event_id.to_named_string() != wire.id {
        return Err(RejectReason::IdMismatch);
    }
    Ok(event_id)
}

/// Map a persisted [`TrustRow`] back to the in-memory [`TrustDecision`] (spec D6
/// restore). A stored code/severity outside the known vocabulary is store
/// corruption, surfaced as a typed error (never a panic on stored bytes).
fn trust_row_to_decision(tr: &TrustRow) -> Result<TrustDecision, StoreError> {
    let code = match tr.code.as_str() {
        "equivocation" => "equivocation",
        "admin_view_suspect" => "admin_view_suspect",
        "store_degraded" => "store_degraded",
        "backfill_depth_exceeded" => "backfill_depth_exceeded",
        "park_overflow" => "park_overflow",
        "admin_tip_expired" => "admin_tip_expired",
        other => {
            return Err(StoreError::integrity(format!(
                "unknown stored trust code {other:?}"
            )))
        }
    };
    let severity = match tr.severity.as_str() {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        other => {
            return Err(StoreError::integrity(format!(
                "unknown stored trust severity {other:?}"
            )))
        }
    };
    Ok(TrustDecision {
        code,
        severity,
        admin_seq: tr.admin_seq.unwrap_or(0),
        event_ids: tr.event_ids.clone(),
    })
}

/// Whether a stored event is **chat-class** (spec §4.1): a chat event type that is
/// **not** admin-authored. Admin-authored events (any type) carry an `admin_seq`
/// and belong to the never-windowed authorization class.
fn is_chat_class(se: &crate::store::StoredEvent) -> bool {
    let chat_ty = matches!(
        se.event_type,
        EventType::MessageText
            | EventType::FileShared
            | EventType::PipeOpened
            | EventType::PipeClosed
            | EventType::AgentStatus
    );
    chat_ty && se.admin_seq.is_none()
}

/// Advisory `created_at >= since_ms` filter (never gates completeness/security —
/// spec §2.3 / R8). With no `since` supplied, always passes; a decode quirk also
/// passes (advisory-only: never drop an event on the time filter).
fn advisory_since(se: &crate::store::StoredEvent, since_ms: Option<u64>) -> bool {
    let Some(since) = since_ms else {
        return true;
    };
    crate::event::signed::SignedEvent::decode(&se.wire.signed)
        .map_or(true, |s| s.created_at >= since)
}

/// Canonical ordering for a response: ascending `(lamport, admin_seq, event_id)`,
/// matching the store's `room_tail` order (spec §6.6).
fn causal_order(
    a: &crate::store::StoredEvent,
    b: &crate::store::StoredEvent,
) -> core::cmp::Ordering {
    a.lamport
        .cmp(&b.lamport)
        .then(a.admin_seq.cmp(&b.admin_seq))
        .then(a.event_id.cmp(&b.event_id))
}
