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
//! (spec D5), so the `events` table stays equal to the convergent validated set.
//! The engine adds: backfill (pull), anti-amplification gating, fan-out, and the
//! admin-tip / fail-closed completeness layer.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::content::{Content, EventType};
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::IdentityKey;
use crate::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
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
}

impl core::fmt::Display for SyncError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "sync_store_error: {e}"),
            Self::InvalidFrame(r) => write!(f, "sync_invalid_frame: {}", r.code()),
            Self::Config(c) => write!(f, "sync_config_error: {c}"),
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
    /// Non-recoverable safety event (admin equivocation).
    Critical,
}

/// A first-class trust event surfaced for the audit surface (spec §9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustDecision {
    /// Stable code: `equivocation` (admin fork) or `admin_view_suspect`.
    pub code: &'static str,
    /// Severity (CRITICAL on an admin fork).
    pub severity: Severity,
    /// The `admin_seq` the decision concerns.
    pub admin_seq: u64,
    /// The event ids involved (both branch tips for a fork).
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

/// Maximum retained log lines (bounded, in-memory; spec §9 / R4).
const MAX_LOG_LINES: usize = 256;

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

        let mut engine = Self {
            room_id,
            config,
            store,
            fold,
            peers: BTreeSet::new(),
            park: BTreeMap::new(),
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

    // ------------------------------------------------------------------
    // Entry points (§6)
    // ------------------------------------------------------------------

    /// Ingest one inbound/fetched `WireEvent` frame (§6.1). Returns frames to send.
    /// A per-frame validation failure is a logged drop, never an error.
    pub fn ingest_frame(&mut self, from: PeerId, bytes: &[u8]) -> Vec<Outgoing> {
        let mut out = Vec::new();
        self.deliver_bytes(Some(from), bytes, &mut out);
        out
    }

    /// Publish a locally-authored, stateless-valid frame (§6.5): ingest it and, on
    /// accept, fan it out to every connected peer.
    ///
    /// # Errors
    /// [`SyncError::InvalidFrame`] if the bytes fail stateless validation.
    pub fn publish(&mut self, bytes: &[u8]) -> Result<Vec<Outgoing>, SyncError> {
        let ctx = ValidationContext::for_room(self.room_id);
        let ev = validate_wire_bytes(bytes, &ctx).map_err(SyncError::InvalidFrame)?;
        let mut out = Vec::new();
        self.deliver(ev, None, &mut out);
        Ok(out)
    }

    /// A peer link came up (§6.3): advertise our admin tip + heads and request the
    /// never-windowed membership sub-DAG and the bounded recent chat window.
    pub fn on_connect(&mut self, peer: PeerId) -> Vec<Outgoing> {
        self.peers.insert(peer);
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
    }

    /// Handle one inbound control/data message (§6.4 responder + the detector).
    pub fn on_message(&mut self, from: PeerId, msg: SyncMessage) -> Vec<Outgoing> {
        let mut out = Vec::new();
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
                for frame in frames {
                    self.deliver_bytes(Some(from), &frame, &mut out);
                }
            }
            SyncMessage::NotFound { ids, .. } => {
                self.log(&format!("peer lacks {} requested ids", ids.len()));
            }
        }
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
        self.refill_tokens();
        self.expire_suspect_tip();
        self.retry_park(&mut out);
        // A restart may have left parked frames owed their one-shot by-id backfill;
        // the first tick after `open` re-issues it if no `on_connect` did (spec §6.3).
        self.retry_restored_park(&mut out);
        let membership_have = self.membership_have();
        let chat_have = self.chat_have();
        for peer in self.peers.iter().copied().collect::<Vec<_>>() {
            out.push(to(peer, self.admin_tip_msg()));
            out.push(to(
                peer,
                SyncMessage::WantMembership {
                    room_id: self.room_id,
                    have: membership_have.clone(),
                },
            ));
            out.push(to(
                peer,
                SyncMessage::WantRecentChat {
                    room_id: self.room_id,
                    window: Window {
                        max_count: self.config.chat_window_default,
                        since_ms: None,
                    },
                    have: chat_have.clone(),
                },
            ));
        }
        out
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
                self.store_and_fanout(&ev, from, out);
                self.wake_park(out);
            }
            crate::membership::Ingest::Buffered { missing, .. } => {
                self.on_buffered(ev, from, &missing, out);
            }
            crate::membership::Ingest::Rejected { reason, .. } => {
                // Fold-rejected (non-member, bad capability, …): counted + logged
                // with a stable `reject.<code>`, stored nowhere, fanned out nowhere
                // (AC3 / spec D8).
                self.counters.rejected += 1;
                self.log(&format!("reject.{}", reason.code()));
            }
        }
    }

    /// Persist an accepted event, update admin state, and fan it out. Does **not**
    /// wake the park (the caller drives that loop to avoid recursion).
    fn store_and_fanout(
        &mut self,
        ev: &ValidatedEvent,
        from: Option<PeerId>,
        out: &mut Vec<Outgoing>,
    ) {
        let id = ev.event_id;
        let outcome = match self.store.insert(ev) {
            Ok(o) => o,
            Err(e) => {
                self.log(&format!("store insert failed: {e}"));
                return;
            }
        };
        match outcome {
            InsertOutcome::Duplicate => {
                self.counters.duplicates += 1;
                // Idempotent re-see: no state change, no re-broadcast storm
                // (spec §8 duplicate-idempotency vector).
            }
            InsertOutcome::Inserted => {
                self.counters.accepted += 1;
                self.note_admin_event(id);
                // Push-subscription feed (issue #83): emit exactly once, only on a real
                // insert (the Duplicate arm never reaches here → exactly-once for free).
                match self.store.get(&id) {
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
        // implausible derived lamport).
        if depth > self.config.max_backfill_depth {
            self.counters.phantom_depth_dropped += 1;
            self.log("reject.phantom_parent_depth");
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
            .filter(|m| !self.store.contains(m).unwrap_or(false))
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
        ev.event
            .prev_events
            .iter()
            .any(|parent| !self.store.contains(parent).unwrap_or(false))
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
    /// persisted park, count and log it.
    fn evict_parked(&mut self, id: EventId, reason: &str) {
        self.park.remove(&id);
        self.backfill_depth.remove(&id);
        self.restored_backfill.remove(&id);
        self.persist_delete_parked(id);
        self.counters.park_evicted += 1;
        self.log(reason);
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
        let pending: Vec<(EventId, IdentityKey, usize)> = self
            .park
            .iter()
            .map(|(id, p)| (*id, p.author, p.depth))
            .collect();
        for (id, author, depth) in pending {
            let missing = match self.store.missing_parents(&id) {
                Ok(m) => m,
                Err(e) => {
                    self.log(&format!("missing_parents failed: {e}"));
                    continue;
                }
            };
            let to_fetch: Vec<EventId> = missing
                .into_iter()
                .filter(|m| !self.store.contains(m).unwrap_or(false))
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
            match self.store.get(id) {
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

    /// Serve the never-windowed authorization-class set, minus what the requester
    /// already holds, in causal order (spec §6.4 — the §0 hard invariant).
    fn serve_want_membership(&mut self, from: PeerId, have: &[EventId], out: &mut Vec<Outgoing>) {
        let have: BTreeSet<EventId> = id_set(have.iter().copied());
        let ids = match self.authorization_class_ids() {
            Ok(s) => s,
            Err(e) => {
                self.log(&format!("authorization-class scan failed: {e}"));
                return;
            }
        };
        let mut stored = Vec::new();
        for id in ids.difference(&have) {
            match self.store.get(id) {
                Ok(Some(se)) => stored.push(se),
                Ok(None) => {}
                Err(e) => self.log(&format!("store get failed: {e}")),
            }
        }
        stored.sort_by(causal_order);
        let frames = self.frames_from_stored(&stored, "WantMembership");
        self.emit_events(from, frames, out);
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

    fn emit_events(&mut self, from: PeerId, frames: Vec<Vec<u8>>, out: &mut Vec<Outgoing>) {
        if frames.is_empty() {
            return;
        }
        self.counters.frames_sent += frames.len() as u64;
        out.push(to(
            from,
            SyncMessage::Events {
                room_id: self.room_id,
                frames,
            },
        ));
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
                && !self.store.contains(&id).unwrap_or(false);
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
            .filter(|id| !self.store.contains(id).unwrap_or(false))
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
                if self.store.contains(id)? {
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
            let still_behind =
                local.map_or(true, |(_, loc)| susp.seq > loc) && !self.store.contains(&susp.id)?;
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
                .filter(|m| !self.store.contains(m).unwrap_or(false))
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
        if let Ok(Some(se)) = self.store.get(&id) {
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

    fn membership_have(&self) -> Vec<EventId> {
        self.authorization_class_ids()
            .map(|s| s.into_iter().collect())
            .unwrap_or_default()
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

/// Map a persisted [`TrustRow`] back to the in-memory [`TrustDecision`] (spec D6
/// restore). A stored code/severity outside the known vocabulary is store
/// corruption, surfaced as a typed error (never a panic on stored bytes).
fn trust_row_to_decision(tr: &TrustRow) -> Result<TrustDecision, StoreError> {
    let code = match tr.code.as_str() {
        "equivocation" => "equivocation",
        "admin_view_suspect" => "admin_view_suspect",
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
