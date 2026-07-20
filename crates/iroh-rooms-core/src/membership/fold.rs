//! The membership fold engine: an in-memory causal DAG of validated events, the
//! ancestor-stable authorization gate, the deterministic per-subject fold, and
//! the ancestor-scoped [`MembershipOracle`] view
//! (`PHASE-0-SPIKE.md` Membership & Ordering §3.4/§3.5/§4; spec D2–D6).
//!
//! [`RoomMembership`] ingests stateless-[`ValidatedEvent`]s in **any** order. An
//! event whose causal parents are not all classified yet is **buffered**
//! (§4 stage 2, no fetch — that is sync's job); once its parents arrive it is
//! re-evaluated. Each event's log-validity is judged **only against its own
//! transitive ancestors** (spec D3), so the accepted set — and therefore the
//! [`MembershipSnapshot`] folded from it — is a pure function of the event set,
//! identical on every peer regardless of arrival order (the §0 same-set
//! convergence guarantee).

use std::collections::{BTreeMap, BTreeSet};

use crate::event::content::{capability_hash, Content, EventType, MemberJoined};
use crate::event::ids::{EventId, RoomId};
use crate::event::keys::{DeviceKey, IdentityKey};
use crate::event::reject::{Flag, MembershipOracle, RejectReason};
use crate::event::signed::SignedEvent;
use crate::event::validate::ValidatedEvent;

use super::model::{Member, MembershipSnapshot, Role, Status, MAX_ACTIVE_MEMBERS};

/// The per-event verdict the fold assigns once the event is causally complete.
#[derive(Clone, Debug)]
enum Verdict {
    /// Causally incomplete — at least one parent is absent or still buffered.
    Pending,
    /// Passed the stateful gate; in the validated set. Carries advisory flags.
    Accepted(Vec<Flag>),
    /// Failed the stateful gate; dropped, never affects state.
    Rejected(RejectReason),
}

/// One node of the causal DAG.
struct Node {
    event: ValidatedEvent,
    verdict: Verdict,
    /// Transitive membership-relevant ancestors, memoized once classified.
    ///
    /// Non-membership events can be numerous (`message.text`, `agent.status`,
    /// files, pipes) but never change membership. Keeping them out of this set
    /// prevents a busy room's causal tail from making the fold `O(total_events^2)`.
    /// Empty while [`Verdict::Pending`].
    membership_ancestors: BTreeSet<EventId>,
}

impl Node {
    fn is_accepted(&self) -> bool {
        matches!(self.verdict, Verdict::Accepted(_))
    }

    fn is_pending(&self) -> bool {
        matches!(self.verdict, Verdict::Pending)
    }
}

/// The outcome of ingesting one event into the fold (spec §7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ingest {
    /// Accepted into the validated set; advisory flags (e.g. `equivocation`,
    /// `clock_skew`) never change the verdict, the set, ordering, or access.
    Accepted {
        /// The accepted event's id.
        event_id: EventId,
        /// Advisory flags attached to the event.
        flags: Vec<Flag>,
    },
    /// Failed the stateful gate; dropped, never affects state.
    Rejected {
        /// The rejected event's id.
        event_id: EventId,
        /// The typed protocol reason.
        reason: RejectReason,
    },
    /// Causally incomplete (a `prev_event` is not yet in the validated set);
    /// buffered, retried when the missing parents arrive. **Not** an error (§4).
    Buffered {
        /// The buffered event's id.
        event_id: EventId,
        /// Parents not yet classified (absent or still buffered).
        missing: Vec<EventId>,
    },
}

/// The deterministic membership fold over an in-memory set of validated events.
///
/// Feed events with [`ingest`](Self::ingest) (any order) or
/// [`from_events`](Self::from_events); read the convergent state with
/// [`snapshot`](Self::snapshot).
pub struct RoomMembership {
    room_id: RoomId,
    nodes: BTreeMap<EventId, Node>,
    /// Reverse edges: parent id → ids that cite it, so a newly-classified parent
    /// can wake its buffered children.
    children: BTreeMap<EventId, Vec<EventId>>,
    /// Accepted structural heads per author, used for equivocation without
    /// retaining full transitive ancestor sets on every node.
    sender_heads: BTreeMap<IdentityKey, BTreeSet<EventId>>,
}

impl RoomMembership {
    /// A fresh, empty fold for `room_id`.
    #[must_use]
    pub fn new(room_id: RoomId) -> Self {
        Self {
            room_id,
            nodes: BTreeMap::new(),
            children: BTreeMap::new(),
            sender_heads: BTreeMap::new(),
        }
    }

    /// The room this fold is bound to.
    #[must_use]
    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// The number of events currently tracked in the causal DAG — accepted,
    /// buffered, or rejected. An observability/test helper for the sync layer's
    /// anti-amplification bound: a frame dropped before [`ingest`](Self::ingest)
    /// never grows this, so a non-member flood must leave it unchanged.
    #[must_use]
    pub fn tracked_event_count(&self) -> usize {
        self.nodes.len()
    }

    /// Fold a whole set in one call (used by tests and the store adapter). Order
    /// is irrelevant — buffering and re-evaluation make the result identical to
    /// any other ingest order over the same set.
    #[must_use]
    pub fn from_events(room_id: RoomId, events: impl IntoIterator<Item = ValidatedEvent>) -> Self {
        let mut membership = Self::new(room_id);
        for event in events {
            membership.ingest(event);
        }
        membership
    }

    /// Ingest one stateless-validated event (any order) and return its outcome.
    ///
    /// Re-ingesting an already-known event is idempotent: it returns the existing
    /// outcome and changes nothing (the stale-replay / dedup case).
    pub fn ingest(&mut self, event: ValidatedEvent) -> Ingest {
        let id = event.event_id;
        if self.nodes.contains_key(&id) {
            return self.outcome(id);
        }
        for parent in &event.event.prev_events {
            self.children.entry(*parent).or_default().push(id);
        }
        self.nodes.insert(
            id,
            Node {
                event,
                verdict: Verdict::Pending,
                membership_ancestors: BTreeSet::new(),
            },
        );

        // Cascade: classify this event and any now-unblocked descendants.
        let mut work = vec![id];
        while let Some(current) = work.pop() {
            if self.try_classify(current) {
                if let Some(kids) = self.children.get(&current) {
                    work.extend(kids.iter().copied());
                }
            }
        }
        self.outcome(id)
    }

    /// The current deterministic fold over the entire local validated set
    /// (spec §5 / §3.4) — the snapshot access decisions consult.
    #[must_use]
    pub fn snapshot(&self) -> MembershipSnapshot {
        let accepted: Vec<EventId> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.is_accepted())
            .map(|(id, _)| *id)
            .collect();
        self.fold(&accepted)
    }

    /// The advisory flags for an already-ingested **accepted** event, evaluated
    /// against the **current** snapshot (spec D6 / §9). This is the home of the
    /// current-snapshot-dependent `from_removed_member` attribution: a log-valid
    /// event whose author has *since* converged to `Removed` is tagged for UI
    /// attribution. It is kept out of the ancestor-stable ingest-time flags on
    /// purpose — "author later removed" depends on events that arrive after the
    /// verdict is fixed, so folding it into [`Ingest::Accepted`] would make that
    /// verdict arrival-order-dependent (R1). Querying it here, against the
    /// current fold, preserves ancestor-stability.
    ///
    /// Returns the event's ingest-time flags (the stateless flags plus any local
    /// `equivocation`) and adds [`Flag::FromRemovedMember`] when the author is
    /// now `Removed`. An unknown or non-accepted event yields no flags. Flags are
    /// **advisory**: they never change a verdict, the validated set, ordering, or
    /// any access decision (§9).
    #[must_use]
    pub fn advisory_flags(&self, event_id: &EventId) -> Vec<Flag> {
        let Some(node) = self.nodes.get(event_id) else {
            return Vec::new();
        };
        let Verdict::Accepted(flags) = &node.verdict else {
            return Vec::new();
        };
        let mut flags = flags.clone();
        let author = node.event.event.sender_id;
        if self.snapshot().status(&author) == Some(Status::Removed)
            && !flags.contains(&Flag::FromRemovedMember)
        {
            flags.push(Flag::FromRemovedMember);
        }
        flags
    }

    /// The ancestor-scoped [`MembershipOracle`] for an already-ingested event,
    /// for re-validation through [`validate_with_membership`](crate::event::validate_with_membership).
    /// `None` if the event was never ingested.
    #[must_use]
    pub fn ancestor_view(&self, event_id: &EventId) -> Option<AncestorView> {
        let node = self.nodes.get(event_id)?;
        let scope = self.accepted_ancestors(&node.membership_ancestors);
        Some(AncestorView {
            snapshot: self.fold(&scope),
        })
    }

    // ------------------------------------------------------------------
    // Cascade / classification
    // ------------------------------------------------------------------

    /// Attempt to move `id` from `Pending` to a final verdict. Returns `true`
    /// iff it transitioned (so its children should be re-evaluated).
    fn try_classify(&mut self, id: EventId) -> bool {
        match self.nodes.get(&id) {
            Some(node) if node.is_pending() => {}
            _ => return false,
        }

        // Readiness: every parent must be present and already classified.
        let parents = self.nodes[&id].event.event.prev_events.clone();
        for parent in &parents {
            match self.nodes.get(parent) {
                Some(node) if !node.is_pending() => {}
                _ => return false,
            }
        }

        // Memoize only membership-relevant ancestors from parents' already
        // memoized sets. Non-membership parents pass through the membership view
        // they inherited, but do not become membership inputs themselves.
        let mut membership_ancestors = BTreeSet::new();
        for parent in &parents {
            if let Some(node) = self.nodes.get(parent) {
                if Self::affects_membership(&node.event.event.content) {
                    membership_ancestors.insert(*parent);
                }
                membership_ancestors.extend(node.membership_ancestors.iter().copied());
            }
        }
        if let Some(node) = self.nodes.get_mut(&id) {
            node.membership_ancestors.clone_from(&membership_ancestors);
        }

        let verdict = self.gate(id, &membership_ancestors);
        let accepted = matches!(verdict, Verdict::Accepted(_));
        if let Some(node) = self.nodes.get_mut(&id) {
            node.verdict = verdict;
        }
        if accepted {
            self.record_sender_head(id);
        }
        true
    }

    /// The outcome to report for an event currently in the fold.
    fn outcome(&self, id: EventId) -> Ingest {
        match self.nodes.get(&id) {
            Some(node) => match &node.verdict {
                Verdict::Accepted(flags) => Ingest::Accepted {
                    event_id: id,
                    flags: flags.clone(),
                },
                Verdict::Rejected(reason) => Ingest::Rejected {
                    event_id: id,
                    reason: reason.clone(),
                },
                Verdict::Pending => Ingest::Buffered {
                    event_id: id,
                    missing: self.blockers(node),
                },
            },
            None => Ingest::Buffered {
                event_id: id,
                missing: Vec::new(),
            },
        }
    }

    /// Parents of `node` that are not yet classified (absent or still buffered).
    fn blockers(&self, node: &Node) -> Vec<EventId> {
        node.event
            .event
            .prev_events
            .iter()
            .copied()
            .filter(|parent| match self.nodes.get(parent) {
                Some(parent_node) => parent_node.is_pending(),
                None => true,
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // The authorization gate (ancestor-based, spec D3 / §6.2)
    // ------------------------------------------------------------------

    /// Decide `id`'s log-validity against its ancestor view, returning the
    /// verdict (accept with advisory flags, or a typed rejection).
    fn gate(&self, id: EventId, membership_ancestors: &BTreeSet<EventId>) -> Verdict {
        let Some(node) = self.nodes.get(&id) else {
            return Verdict::Rejected(RejectReason::NotAMember);
        };
        let event = &node.event.event;

        // Advisory flags: carry the stateless ones through, add local equivocation.
        let mut flags = node.event.flags.clone();
        if self.is_equivocation(id, event.sender_id) && !flags.contains(&Flag::Equivocation) {
            flags.push(Flag::Equivocation);
        }

        let scope = self.accepted_ancestors(membership_ancestors);
        let view = self.fold(&scope);

        // Each arm is a distinct §6.2 gate rule; some happen to share the `Ok(())`
        // verdict (genesis self-authorizes; self-leave is always valid). Keep them
        // separate for clarity rather than merging unrelated rules.
        #[allow(clippy::match_same_arms)]
        let result = match &event.content {
            // Genesis: structure already verified statelessly; it seeds the admin.
            Content::RoomCreated(_) => Ok(()),
            // Admin-only authorization writes.
            Content::MemberInvited(_) | Content::MemberRemoved(_) => {
                Self::gate_admin_action(event, &view)
            }
            // Self-departure is always valid (may be a no-op / inert).
            Content::MemberLeft(_) => Ok(()),
            // The full key-bound join gate (spec D4 / §3.5).
            Content::MemberJoined(content) => {
                self.gate_join(id, event, content, membership_ancestors, &view)
            }
            // Non-membership writes: author must be Active in the ancestor view.
            Content::MessageText(_)
            | Content::FileShared(_)
            | Content::PipeOpened(_)
            | Content::PipeClosed(_)
            | Content::AgentStatus(_) => Self::gate_active_member(event, &view),
        };

        // Membership-derived device binding (step 7), after authorization (step 8)
        // so a non-member yields `not_a_member` rather than `unbound_device`.
        let result = result.and_then(|()| Self::gate_device_binding(event, &view));

        match result {
            Ok(()) => Verdict::Accepted(flags),
            Err(reason) => Verdict::Rejected(reason),
        }
    }

    /// `member.invited` / `member.removed`: valid iff the signer is the single
    /// immutable admin. (`member.removed`'s `member_id != admin` is guaranteed by
    /// the stateless `member_id != sender_id` rule, since `sender_id == admin`.)
    fn gate_admin_action(
        event: &SignedEvent,
        view: &MembershipSnapshot,
    ) -> Result<(), RejectReason> {
        if view.admin() == Some(&event.sender_id) {
            Ok(())
        } else {
            Err(RejectReason::InsufficientRole)
        }
    }

    /// Non-membership writes: the author must be `Active` in the ancestor view.
    fn gate_active_member(
        event: &SignedEvent,
        view: &MembershipSnapshot,
    ) -> Result<(), RejectReason> {
        if view.is_active(&event.sender_id) {
            Ok(())
        } else {
            Err(RejectReason::NotAMember)
        }
    }

    /// Device binding from membership state (step 7), for the types that carry no
    /// self-contained binding. A subject with no membership-bound device (e.g. an
    /// inert `member.left` from a non-member) is accepted — it grants nothing.
    fn gate_device_binding(
        event: &SignedEvent,
        view: &MembershipSnapshot,
    ) -> Result<(), RejectReason> {
        if !event.event_type.requires_membership_device_binding() {
            return Ok(());
        }
        match view.member(&event.sender_id).and_then(|m| m.device) {
            Some(bound) if bound == event.device_id => Ok(()),
            Some(_) => Err(RejectReason::UnboundDevice),
            None => Ok(()),
        }
    }

    /// The key-bound join gate (spec D4 / §3.5 / §6): a live, key-bound,
    /// capability-matching, role-matching, unexpired, un-consumed admin invite
    /// must exist in the join's causal ancestors.
    fn gate_join(
        &self,
        _join_id: EventId,
        event: &SignedEvent,
        content: &MemberJoined,
        membership_ancestors: &BTreeSet<EventId>,
        view: &MembershipSnapshot,
    ) -> Result<(), RejectReason> {
        let subject = event.sender_id;
        // No genesis in the ancestor view => no admin => no valid invite.
        let Some(admin) = view.admin().copied() else {
            return Err(RejectReason::BadCapability);
        };

        // Find the naming, key-bound, capability-matching admin invite in scope.
        // Bearer/open tickets are excluded from MVP: a join under a key with no
        // naming invite fails here, so ban-evasion under a fresh key is blocked.
        let mut matched: Option<EventId> = None;
        for ancestor_id in membership_ancestors {
            let Some(node) = self.nodes.get(ancestor_id) else {
                continue;
            };
            if !node.is_accepted() {
                continue;
            }
            let Content::MemberInvited(invite) = &node.event.event.content else {
                continue;
            };
            if invite.invite_id != content.via_invite_id
                || invite.invitee_key != subject
                || node.event.event.sender_id != admin
            {
                continue;
            }
            // Recompute and match the key-bound capability hash (§6).
            let recomputed = capability_hash(
                &self.room_id,
                &content.via_invite_id,
                &content.capability_secret,
            );
            if recomputed != invite.capability_hash {
                continue;
            }
            // Log-only expiry: signed expires_at vs signed created_at (never the
            // local clock, §6), so every peer computes the same verdict.
            if let Some(expiry) = invite.expires_at {
                if event.created_at > expiry {
                    return Err(RejectReason::ExpiredInvite);
                }
            }
            // Join role must equal the invite's role (§3.5).
            if Role::from_validated_str(&content.role) != Role::from_validated_str(&invite.role) {
                return Err(RejectReason::InsufficientRole);
            }
            // Deterministically prefer the lowest-id matching invite.
            matched = Some(match matched {
                Some(existing) if existing <= *ancestor_id => existing,
                _ => *ancestor_id,
            });
        }

        let invite_id = matched.ok_or(RejectReason::BadCapability)?;

        // Sticky departure (§3.7): the invite is consumed if any departure of the
        // subject in the join's ancestors causally descends from that invite.
        if self.departure_consumes(subject, invite_id, membership_ancestors) {
            return Err(RejectReason::ExpiredInvite);
        }

        if !view.is_active(&subject) && view.active_member_count() >= MAX_ACTIVE_MEMBERS {
            return Err(RejectReason::RoomFull);
        }

        Ok(())
    }

    /// Whether a `member.removed`/`member.left` of `subject` in `ancestors`
    /// causally descends from `invite_id` (consuming it, spec D4 / §3.7).
    fn departure_consumes(
        &self,
        subject: IdentityKey,
        invite_id: EventId,
        membership_ancestors: &BTreeSet<EventId>,
    ) -> bool {
        membership_ancestors.iter().any(|ancestor_id| {
            let Some(node) = self.nodes.get(ancestor_id) else {
                return false;
            };
            if !node.is_accepted() {
                return false;
            }
            let is_departure = match &node.event.event.content {
                Content::MemberRemoved(c) => c.member_id == subject,
                Content::MemberLeft(c) => c.member_id == subject,
                _ => false,
            };
            is_departure && node.membership_ancestors.contains(&invite_id)
        })
    }

    /// Local equivocation detection: `sender` already authored an accepted event
    /// that is **concurrent** with the one being classified (neither is an
    /// ancestor of the other). Advisory only — never changes a verdict or the
    /// snapshot (§9).
    fn is_equivocation(&self, id: EventId, sender: IdentityKey) -> bool {
        self.sender_heads.get(&sender).is_some_and(|heads| {
            heads
                .iter()
                .any(|head| !self.is_structural_ancestor(*head, id))
        })
    }

    /// Record a newly accepted event as one of the sender's structural causal
    /// heads, dropping any previous head this event descends from.
    fn record_sender_head(&mut self, id: EventId) {
        let Some(sender) = self.nodes.get(&id).map(|node| node.event.event.sender_id) else {
            return;
        };
        let existing = self.sender_heads.get(&sender).cloned().unwrap_or_default();
        let mut retained: BTreeSet<EventId> = existing
            .into_iter()
            .filter(|head| !self.is_structural_ancestor(*head, id))
            .collect();
        retained.insert(id);
        self.sender_heads.insert(sender, retained);
    }

    /// Whether `ancestor` is a structural ancestor of `descendant` through
    /// `prev_events`. This is used only for same-sender head maintenance and
    /// equivocation, so the fold no longer needs per-node full ancestor sets.
    fn is_structural_ancestor(&self, ancestor: EventId, descendant: EventId) -> bool {
        let mut stack = match self.nodes.get(&descendant) {
            Some(node) => node.event.event.prev_events.clone(),
            None => return false,
        };
        let mut seen = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            if current == ancestor {
                return true;
            }
            if let Some(node) = self.nodes.get(&current) {
                stack.extend(node.event.event.prev_events.iter().copied());
            }
        }
        false
    }

    fn affects_membership(content: &Content) -> bool {
        matches!(
            content,
            Content::RoomCreated(_)
                | Content::MemberInvited(_)
                | Content::MemberJoined(_)
                | Content::MemberLeft(_)
                | Content::MemberRemoved(_)
        )
    }

    // ------------------------------------------------------------------
    // The fold (spec D5 / §3.4)
    // ------------------------------------------------------------------

    /// The accepted subset of `ancestors`, in deterministic id order.
    fn accepted_ancestors(&self, ancestors: &BTreeSet<EventId>) -> Vec<EventId> {
        ancestors
            .iter()
            .copied()
            .filter(|id| self.nodes.get(id).is_some_and(Node::is_accepted))
            .collect()
    }

    /// Fold a set of accepted event ids into a [`MembershipSnapshot`] (spec D5).
    /// Pure: depends only on the set, never on arrival order.
    fn fold(&self, scope: &[EventId]) -> MembershipSnapshot {
        let mut members: BTreeMap<IdentityKey, Member> = BTreeMap::new();
        let mut by_device: BTreeMap<DeviceKey, IdentityKey> = BTreeMap::new();
        let mut admin: Option<IdentityKey> = None;

        // Seed the immutable admin from the genesis (one per room).
        for id in scope {
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            if matches!(node.event.event.content, Content::RoomCreated(_)) {
                let identity = node.event.event.sender_id;
                let device = node.event.event.device_id;
                admin = Some(identity);
                members.insert(
                    identity,
                    Member {
                        identity,
                        device: Some(device),
                        status: Status::Active,
                        role: Role::Admin,
                    },
                );
                by_device.insert(device, identity);
                break;
            }
        }

        // Collect every touched subject (excluding the admin, who is immutable).
        let mut subjects: BTreeSet<IdentityKey> = BTreeSet::new();
        for id in scope {
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            match &node.event.event.content {
                Content::MemberInvited(c) => {
                    subjects.insert(c.invitee_key);
                }
                Content::MemberJoined(_) => {
                    subjects.insert(node.event.event.sender_id);
                }
                Content::MemberLeft(c) => {
                    subjects.insert(c.member_id);
                }
                Content::MemberRemoved(c) => {
                    subjects.insert(c.member_id);
                }
                _ => {}
            }
        }
        if let Some(a) = admin {
            subjects.remove(&a);
        }

        let mut active_ranks = BTreeMap::new();
        for subject in subjects {
            let touch = self.touch_events(scope, subject);
            if touch.is_empty() {
                continue;
            }
            let heads = self.causal_heads(&touch);
            let status = self.status_from_heads(&heads);
            if status == Status::Active {
                if let Some(rank) = self.activation_event(&touch, &heads) {
                    active_ranks.insert(subject, rank);
                }
            }
            let role = self.resolve_role(&touch, &heads);
            let device = self.resolve_device(&touch, &heads);
            members.insert(
                subject,
                Member {
                    identity: subject,
                    device,
                    status,
                    role,
                },
            );
            if let Some(d) = device {
                by_device.insert(d, subject);
            }
        }

        Self::cap_active_members(admin, &mut members, &active_ranks);

        MembershipSnapshot::new(self.room_id, admin, members, by_device)
    }

    /// The accepted events in `scope` that touch `subject` (spec D5 step 1).
    fn touch_events(&self, scope: &[EventId], subject: IdentityKey) -> Vec<EventId> {
        scope
            .iter()
            .copied()
            .filter(|id| {
                let Some(node) = self.nodes.get(id) else {
                    return false;
                };
                match &node.event.event.content {
                    Content::MemberInvited(c) => c.invitee_key == subject,
                    Content::MemberJoined(_) => node.event.event.sender_id == subject,
                    Content::MemberLeft(c) => c.member_id == subject,
                    Content::MemberRemoved(c) => c.member_id == subject,
                    _ => false,
                }
            })
            .collect()
    }

    /// Causal heads of `touch`: events with no other event of the set among their
    /// descendants (spec D5 step 2).
    fn causal_heads(&self, touch: &[EventId]) -> Vec<EventId> {
        touch
            .iter()
            .copied()
            .filter(|head| {
                !touch
                    .iter()
                    .any(|other| other != head && self.is_ancestor(*head, *other))
            })
            .collect()
    }

    /// Whether `ancestor` is a (strict) causal ancestor of `descendant`.
    fn is_ancestor(&self, ancestor: EventId, descendant: EventId) -> bool {
        self.nodes
            .get(&descendant)
            .is_some_and(|node| node.membership_ancestors.contains(&ancestor))
    }

    /// Removed-dominates status over the heads (spec D5 step 3): the `max` over
    /// each head's status contribution.
    fn status_from_heads(&self, heads: &[EventId]) -> Status {
        let mut status = Status::Invited;
        for head in heads {
            let Some(node) = self.nodes.get(head) else {
                continue;
            };
            let contribution = match &node.event.event.content {
                Content::MemberRemoved(_) | Content::MemberLeft(_) => Status::Removed,
                Content::MemberJoined(_) => Status::Active,
                Content::MemberInvited(_) => Status::Invited,
                _ => continue,
            };
            status = status.max(contribution);
        }
        status
    }

    /// Least-privilege role merge (spec D5 step 4 / §3.8): the `min` over the
    /// role-carrying heads, tie-broken by lowest `event_id`. Falls back to the
    /// role-carrying `touch` events, then to [`Role::Member`] (a subject removed
    /// without ever being invited/joined carries no role head).
    fn resolve_role(&self, touch: &[EventId], heads: &[EventId]) -> Role {
        self.least_privilege_role(heads)
            .or_else(|| self.least_privilege_role(touch))
            .unwrap_or(Role::Member)
    }

    /// The least-privilege role among the role-carrying events of `ids`
    /// (`member.invited` / `member.joined`), tie-broken by lowest `event_id`.
    fn least_privilege_role(&self, ids: &[EventId]) -> Option<Role> {
        ids.iter()
            .filter_map(|id| {
                let node = self.nodes.get(id)?;
                let role = match &node.event.event.content {
                    Content::MemberInvited(c) => Role::from_validated_str(&c.role),
                    Content::MemberJoined(c) => Role::from_validated_str(&c.role),
                    _ => return None,
                };
                Some((role, *id))
            })
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(role, _)| role)
    }

    fn activation_event(&self, touch: &[EventId], heads: &[EventId]) -> Option<EventId> {
        heads
            .iter()
            .chain(touch.iter())
            .filter_map(|id| {
                let node = self.nodes.get(id)?;
                matches!(node.event.event.content, Content::MemberJoined(_)).then_some(*id)
            })
            .min()
    }

    fn cap_active_members(
        admin: Option<IdentityKey>,
        members: &mut BTreeMap<IdentityKey, Member>,
        active_ranks: &BTreeMap<IdentityKey, EventId>,
    ) {
        let slots = MAX_ACTIVE_MEMBERS.saturating_sub(usize::from(admin.is_some()));
        let mut admitted: BTreeSet<IdentityKey> = active_ranks
            .iter()
            .map(|(identity, rank)| (*rank, *identity))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(slots)
            .map(|(_, identity)| identity)
            .collect();
        if let Some(admin) = admin {
            admitted.insert(admin);
        }
        for member in members.values_mut() {
            if member.status == Status::Active && !admitted.contains(&member.identity) {
                member.status = Status::Invited;
            }
        }
    }

    /// The device bound to the subject: from the lowest-id `member.joined` head
    /// if one exists (the current device), else from the lowest-id join anywhere
    /// in `touch` (a since-departed subject's last-known device), else `None`.
    fn resolve_device(&self, touch: &[EventId], heads: &[EventId]) -> Option<DeviceKey> {
        self.lowest_join_device(heads)
            .or_else(|| self.lowest_join_device(touch))
    }

    /// The `device_id` of the lowest-`event_id` `member.joined` among `ids`.
    fn lowest_join_device(&self, ids: &[EventId]) -> Option<DeviceKey> {
        ids.iter()
            .filter(|id| {
                self.nodes.get(id).is_some_and(|node| {
                    matches!(node.event.event.content, Content::MemberJoined(_))
                })
            })
            .min()
            .and_then(|id| self.nodes.get(id))
            .map(|node| node.event.event.device_id)
    }
}

/// An ancestor-scoped [`MembershipOracle`] over one event's causal ancestors
/// (spec D3): the fold restricted to those ancestors. Implementing the frozen
/// trait against the ancestor view (never live state) is what makes log-validity
/// ancestor-stable and convergent.
///
/// Obtained from [`RoomMembership::ancestor_view`]. The `member.joined`
/// capability check is **not** expressible through the content-free trait and is
/// handled by the fold's ingest path instead (spec Open Q1).
pub struct AncestorView {
    snapshot: MembershipSnapshot,
}

impl AncestorView {
    /// The folded membership snapshot over the scoped ancestors.
    #[must_use]
    pub fn snapshot(&self) -> &MembershipSnapshot {
        &self.snapshot
    }
}

impl MembershipOracle for AncestorView {
    fn bound_device(&self, room_id: &RoomId, sender_id: &IdentityKey) -> Option<[u8; 32]> {
        if room_id != self.snapshot.room_id() {
            return None;
        }
        self.snapshot
            .member(sender_id)
            .and_then(|m| m.device)
            .map(|d| *d.as_bytes())
    }

    fn authorize(
        &self,
        room_id: &RoomId,
        sender_id: &IdentityKey,
        event_type: &str,
    ) -> Result<(), RejectReason> {
        if room_id != self.snapshot.room_id() {
            return Err(RejectReason::NotAMember);
        }
        // Each arm is a distinct §6.2 gate rule; some share the `Ok(())` verdict.
        #[allow(clippy::match_same_arms)]
        match EventType::from_registry(event_type) {
            // Genesis authorizes itself structurally (no prior state).
            Some(EventType::RoomCreated) => Ok(()),
            // Admin-only authorization writes.
            Some(EventType::MemberInvited | EventType::MemberRemoved) => {
                if self.snapshot.admin() == Some(sender_id) {
                    Ok(())
                } else {
                    Err(RejectReason::InsufficientRole)
                }
            }
            // Self-departure is always valid.
            Some(EventType::MemberLeft) => Ok(()),
            // The capability check is membership-internal (spec Open Q1); the
            // trait deliberately does not gate joins on its own.
            Some(EventType::MemberJoined) => Ok(()),
            // Non-membership writes require an Active author.
            Some(
                EventType::MessageText
                | EventType::FileShared
                | EventType::PipeOpened
                | EventType::PipeClosed
                | EventType::AgentStatus,
            ) => {
                if self.snapshot.is_active(sender_id) {
                    Ok(())
                } else {
                    Err(RejectReason::NotAMember)
                }
            }
            // Unknown types never reach here (stateless layer rejects them); fail closed.
            None => Err(RejectReason::NotAMember),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ingest, RoomMembership};
    use crate::event::binding::DeviceBinding;
    use crate::event::capability_hash;
    use crate::event::content::{
        Content, EventType, MemberInvited, MemberJoined, MessageText, RoomCreated,
    };
    use crate::event::ids::RoomId;
    use crate::event::keys::SigningKey;
    use crate::event::reject::RejectReason;
    use crate::event::signed::{self, SignedEvent};
    use crate::event::validate::{validate_wire_bytes, ValidatedEvent, ValidationContext};
    use crate::event::wire::WireEvent;
    use crate::membership::Status;

    const NONCE: [u8; 16] = [0xaa; 16];
    const T0: u64 = 1_750_000_000_000;
    const INVITE_ID: [u8; 16] = [0xda; 16];
    const SECRET: [u8; 16] = [0x5e; 16];

    fn sk(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; 32])
    }

    fn ctx(room: RoomId) -> ValidationContext {
        ValidationContext::for_room(room)
    }

    fn seal(ev: &SignedEvent, dev: &SigningKey) -> Vec<u8> {
        let csb = ev.to_csb();
        let sig = signed::sign_csb(&csb, dev);
        WireEvent::seal(csb, sig).to_bytes()
    }

    fn make_genesis(admin_id: &SigningKey, admin_dev: &SigningKey) -> (ValidatedEvent, RoomId) {
        let id_key = admin_id.identity_key();
        let dev_key = admin_dev.device_key();
        let room = signed::derive_room_id(&id_key, &NONCE, T0);
        let binding = DeviceBinding::create(&room, admin_id, dev_key);
        let ev = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: id_key,
            device_id: dev_key,
            event_type: EventType::RoomCreated,
            created_at: T0,
            prev_events: vec![],
            content: Content::RoomCreated(RoomCreated {
                room_name: "Room".to_owned(),
                room_nonce: NONCE,
                admins: vec![id_key],
                device_binding: binding,
            }),
        };
        let v = validate_wire_bytes(&seal(&ev, admin_dev), &ctx(room)).expect("genesis valid");
        (v, room)
    }

    #[allow(clippy::too_many_arguments)]
    fn make_invite(
        admin_id: &SigningKey,
        admin_dev: &SigningKey,
        room: RoomId,
        invitee: &SigningKey,
        invite_id: [u8; 16],
        cap_hash: [u8; 32],
        expires_at: Option<u64>,
        prev: crate::event::ids::EventId,
        t: u64,
    ) -> ValidatedEvent {
        let ev = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: admin_id.identity_key(),
            device_id: admin_dev.device_key(),
            event_type: EventType::MemberInvited,
            created_at: t,
            prev_events: vec![prev],
            content: Content::MemberInvited(MemberInvited {
                invite_id,
                capability_hash: cap_hash,
                role: "member".to_owned(),
                invitee_key: invitee.identity_key(),
                expires_at,
                invitee_hint: None,
            }),
        };
        validate_wire_bytes(&seal(&ev, admin_dev), &ctx(room)).expect("invite stateless-valid")
    }

    fn make_join(
        invitee: &SigningKey,
        invitee_dev: &SigningKey,
        room: RoomId,
        invite_id: [u8; 16],
        secret: [u8; 16],
        prev: crate::event::ids::EventId,
        t: u64,
    ) -> ValidatedEvent {
        let id_key = invitee.identity_key();
        let dev_key = invitee_dev.device_key();
        let binding = DeviceBinding::create(&room, invitee, dev_key);
        let ev = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: id_key,
            device_id: dev_key,
            event_type: EventType::MemberJoined,
            created_at: t,
            prev_events: vec![prev],
            content: Content::MemberJoined(MemberJoined {
                via_invite_id: invite_id,
                capability_secret: secret,
                role: "member".to_owned(),
                device_binding: binding,
                display_name: None,
            }),
        };
        validate_wire_bytes(&seal(&ev, invitee_dev), &ctx(room)).expect("join stateless-valid")
    }

    fn make_message(
        sender_id: &SigningKey,
        sender_dev: &SigningKey,
        room: RoomId,
        prev: crate::event::ids::EventId,
        body: &str,
        t: u64,
    ) -> ValidatedEvent {
        let ev = SignedEvent {
            schema_version: 1,
            room_id: room,
            sender_id: sender_id.identity_key(),
            device_id: sender_dev.device_key(),
            event_type: EventType::MessageText,
            created_at: t,
            prev_events: vec![prev],
            content: Content::MessageText(MessageText {
                body: body.to_owned(),
                format: None,
                in_reply_to: None,
                mentions: None,
            }),
        };
        validate_wire_bytes(&seal(&ev, sender_dev), &ctx(room)).expect("message stateless-valid")
    }

    // AC1 (positive): valid admin invite is accepted by the fold and marks the
    // invitee as Invited.
    #[test]
    fn admin_invite_accepted_and_invitee_status_is_invited() {
        let (admin_id, admin_dev) = (sk(1), sk(2));
        let invitee = sk(4);
        let (genesis, room) = make_genesis(&admin_id, &admin_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        let invite = make_invite(
            &admin_id,
            &admin_dev,
            room,
            &invitee,
            INVITE_ID,
            cap_hash,
            None,
            genesis.event_id,
            T0 + 1,
        );
        let invite_outcome = {
            let mut m = RoomMembership::new(room);
            m.ingest(genesis.clone());
            m.ingest(invite.clone())
        };
        assert!(
            matches!(invite_outcome, Ingest::Accepted { .. }),
            "admin invite must be accepted by the fold; got {invite_outcome:?}"
        );
        let snap = RoomMembership::from_events(room, [genesis, invite]).snapshot();
        assert_eq!(
            snap.status(&invitee.identity_key()),
            Some(Status::Invited),
            "invitee must be Invited in the snapshot after admin invite"
        );
    }

    // AC1 (negative): a member.invited signed by a non-admin is rejected by the
    // fold with InsufficientRole. The stateless layer passes it (no admin check
    // there); the fold's gate_admin_action is the enforcement point.
    #[test]
    fn non_admin_invite_rejected_with_insufficient_role() {
        let (alice_id, alice_dev) = (sk(1), sk(2)); // admin
        let (bob_id, bob_dev) = (sk(3), sk(4)); // non-admin, tries to invite
        let invitee = sk(5);
        let (genesis, room) = make_genesis(&alice_id, &alice_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        // Build the invite signed by bob (not the room admin).
        let non_admin_invite = {
            let ev = SignedEvent {
                schema_version: 1,
                room_id: room,
                sender_id: bob_id.identity_key(),
                device_id: bob_dev.device_key(),
                event_type: EventType::MemberInvited,
                created_at: T0 + 1,
                prev_events: vec![genesis.event_id],
                content: Content::MemberInvited(MemberInvited {
                    invite_id: INVITE_ID,
                    capability_hash: cap_hash,
                    role: "member".to_owned(),
                    invitee_key: invitee.identity_key(),
                    expires_at: None,
                    invitee_hint: None,
                }),
            };
            validate_wire_bytes(&seal(&ev, &bob_dev), &ctx(room))
                .expect("passes stateless validation")
        };
        let mut membership = RoomMembership::new(room);
        membership.ingest(genesis);
        let outcome = membership.ingest(non_admin_invite);
        assert!(
            matches!(
                outcome,
                Ingest::Rejected {
                    reason: RejectReason::InsufficientRole,
                    ..
                }
            ),
            "non-admin invite must be rejected with InsufficientRole; got {outcome:?}"
        );
        // The would-be invitee must have no membership entry.
        assert_eq!(
            membership.snapshot().status(&invitee.identity_key()),
            None,
            "invitee must have no status after a rejected invite"
        );
    }

    // AC5 (expiry): a member.joined whose created_at exceeds the invite's
    // expires_at is rejected by the fold with ExpiredInvite.
    #[test]
    fn join_after_invite_expiry_rejected() {
        let (admin_id, admin_dev) = (sk(1), sk(2));
        let (invitee, invitee_dev) = (sk(4), sk(5));
        let (genesis, room) = make_genesis(&admin_id, &admin_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        let expiry = T0 + 1_000; // expires 1 s after genesis
        let invite = make_invite(
            &admin_id,
            &admin_dev,
            room,
            &invitee,
            INVITE_ID,
            cap_hash,
            Some(expiry),
            genesis.event_id,
            T0 + 1,
        );
        // Join at T0 + 2_000: strictly after the expiry (T0 + 1_000).
        let join = make_join(
            &invitee,
            &invitee_dev,
            room,
            INVITE_ID,
            SECRET,
            invite.event_id,
            T0 + 2_000,
        );
        let mut membership = RoomMembership::new(room);
        membership.ingest(genesis);
        membership.ingest(invite);
        let outcome = membership.ingest(join);
        assert!(
            matches!(
                outcome,
                Ingest::Rejected {
                    reason: RejectReason::ExpiredInvite,
                    ..
                }
            ),
            "join after invite expiry must be rejected with ExpiredInvite; got {outcome:?}"
        );
    }

    // AC4: join with a wrong capability secret is rejected with BadCapability
    // (the recomputed hash does not match the invite's capability_hash).
    #[test]
    fn join_with_wrong_capability_secret_rejected() {
        let (admin_id, admin_dev) = (sk(1), sk(2));
        let (invitee, invitee_dev) = (sk(4), sk(5));
        let (genesis, room) = make_genesis(&admin_id, &admin_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        let invite = make_invite(
            &admin_id,
            &admin_dev,
            room,
            &invitee,
            INVITE_ID,
            cap_hash,
            None,
            genesis.event_id,
            T0 + 1,
        );
        // Use a different secret in the join — the hash won't match.
        let wrong_secret: [u8; 16] = [0xff; 16];
        let join = make_join(
            &invitee,
            &invitee_dev,
            room,
            INVITE_ID,
            wrong_secret,
            invite.event_id,
            T0 + 2,
        );
        let mut membership = RoomMembership::new(room);
        membership.ingest(genesis);
        membership.ingest(invite);
        let outcome = membership.ingest(join);
        assert!(
            matches!(
                outcome,
                Ingest::Rejected {
                    reason: RejectReason::BadCapability,
                    ..
                }
            ),
            "join with wrong secret must be rejected with BadCapability; got {outcome:?}"
        );
    }

    // AC2 (positive): a valid join (correct key + secret + unexpired invite)
    // transitions the invitee to Active.
    #[test]
    fn valid_join_transitions_invitee_to_active() {
        let (admin_id, admin_dev) = (sk(1), sk(2));
        let (invitee, invitee_dev) = (sk(4), sk(5));
        let (genesis, room) = make_genesis(&admin_id, &admin_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        let invite = make_invite(
            &admin_id,
            &admin_dev,
            room,
            &invitee,
            INVITE_ID,
            cap_hash,
            None,
            genesis.event_id,
            T0 + 1,
        );
        let join = make_join(
            &invitee,
            &invitee_dev,
            room,
            INVITE_ID,
            SECRET,
            invite.event_id,
            T0 + 2,
        );
        let snap = RoomMembership::from_events(room, [genesis, invite, join]).snapshot();
        assert_eq!(
            snap.status(&invitee.identity_key()),
            Some(Status::Active),
            "invitee must be Active after a valid join"
        );
    }

    #[test]
    fn non_membership_tail_does_not_expand_membership_ancestor_cache() {
        let (admin_id, admin_dev) = (sk(1), sk(2));
        let (member_id, member_dev) = (sk(4), sk(5));
        let (genesis, room) = make_genesis(&admin_id, &admin_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        let invite = make_invite(
            &admin_id,
            &admin_dev,
            room,
            &member_id,
            INVITE_ID,
            cap_hash,
            None,
            genesis.event_id,
            T0 + 1,
        );
        let join = make_join(
            &member_id,
            &member_dev,
            room,
            INVITE_ID,
            SECRET,
            invite.event_id,
            T0 + 2,
        );
        let mut membership =
            RoomMembership::from_events(room, [genesis.clone(), invite.clone(), join.clone()]);
        let mut prev = join.event_id;

        for i in 0..1_024_u64 {
            let msg = make_message(
                &member_id,
                &member_dev,
                room,
                prev,
                &format!("status {i}"),
                T0 + 3 + i,
            );
            let outcome = membership.ingest(msg.clone());
            assert!(
                matches!(outcome, Ingest::Accepted { .. }),
                "message {i} must be accepted; got {outcome:?}"
            );
            let node = membership
                .nodes
                .get(&msg.event_id)
                .expect("message node must exist");
            assert_eq!(
                node.membership_ancestors.len(),
                3,
                "message {i} must inherit only genesis + invite + join as membership ancestors"
            );
            let heads = membership
                .sender_heads
                .get(&member_id.identity_key())
                .expect("accepted member event must be a sender head");
            assert_eq!(
                heads.len(),
                1,
                "sequential same-sender messages must keep one structural head"
            );
            prev = msg.event_id;
        }
    }

    // AC2 (negative): a join that cites the correct invite but uses the wrong
    // identity key (different sender_id) is rejected — the invite is key-bound.
    #[test]
    fn join_by_wrong_identity_rejected() {
        let (admin_id, admin_dev) = (sk(1), sk(2));
        let invitee = sk(4);
        let (impersonator, impersonator_dev) = (sk(6), sk(7)); // not the invited key
        let (genesis, room) = make_genesis(&admin_id, &admin_dev);
        let cap_hash = capability_hash(&room, &INVITE_ID, &SECRET);
        let invite = make_invite(
            &admin_id,
            &admin_dev,
            room,
            &invitee, // bound to invitee's key
            INVITE_ID,
            cap_hash,
            None,
            genesis.event_id,
            T0 + 1,
        );
        // Impersonator tries to join using the same invite_id + correct secret.
        let join = make_join(
            &impersonator,
            &impersonator_dev,
            room,
            INVITE_ID,
            SECRET,
            invite.event_id,
            T0 + 2,
        );
        let mut membership = RoomMembership::new(room);
        membership.ingest(genesis);
        membership.ingest(invite);
        let outcome = membership.ingest(join);
        assert!(
            matches!(
                outcome,
                Ingest::Rejected {
                    reason: RejectReason::BadCapability,
                    ..
                }
            ),
            "join by wrong identity must be rejected; got {outcome:?}"
        );
    }
}
