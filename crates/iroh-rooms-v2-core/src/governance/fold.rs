//! The deterministic governance fold (spec §4 D4 / §6.4 / #147 / #148).
//!
//! [`GovernanceFold`] consumes verified governance entries, approvals, and fork
//! resolutions **in any order** and converges to a single [`FoldOutcome`]. The
//! outcome is a pure function of the input *set*: identical input sets produce
//! byte-identical state roots and member projections, regardless of arrival
//! order (spec §4 D4 / §11 reliability).
//!
//! The fold is a fixpoint: it repeatedly applies any entry whose preconditions
//! are satisfied until no further progress is possible. Unapplied entries are
//! either rejected (typed reason) or buffered as missing-dependency. Forks are
//! detected and recorded; an unresolved fork fails the author's authorization
//! closed (spec D6) until a valid `fork.resolve` is applied.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Reject;
use crate::governance::approval::ApprovalBody;
use crate::governance::authz;
use crate::governance::fork::{self, ForkResolutionBody, ForkResolveAction};
use crate::governance::model::{
    GovernanceAction, GovernanceEntryBody, GovernanceState, MemberRecord, MemberStatus, Role,
};
use crate::ids::{GovernanceEntryId, StateRoot};
use crate::member::projection;

/// One accepted/rejected item in the fold outcome (spec §6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldItem {
    /// Accepted into the governance state.
    Accepted(GovernanceEntryId),
    /// Rejected with a typed reason; never affects state.
    Rejected {
        /// The rejected entry id.
        id: GovernanceEntryId,
        /// The typed reason.
        reason: Reject,
    },
}

/// The result of folding a set of governance records (spec §6.4 / §6.5).
#[derive(Debug, Clone)]
pub struct FoldOutcome {
    /// The room this fold is for.
    pub room_id: crate::ids::RoomId,
    /// Per-entry accept/reject decisions, in deterministic id order.
    pub items: Vec<FoldItem>,
    /// Unresolved fork evidence carried in the state (spec D6).
    pub unresolved_forks: Vec<crate::governance::model::ForkEvidence>,
    /// The final folded governance state.
    pub state: GovernanceState,
    /// The recomputed state root (commits to state + member root + forks).
    pub state_root: StateRoot,
    /// The recomputed member Merkle root.
    pub member_root: crate::ids::MerkleRoot,
}

impl FoldOutcome {
    /// The accepted entry ids.
    #[must_use]
    pub fn accepted_ids(&self) -> Vec<GovernanceEntryId> {
        self.items
            .iter()
            .filter_map(|i| match i {
                FoldItem::Accepted(id) => Some(*id),
                FoldItem::Rejected { .. } => None,
            })
            .collect()
    }
}

/// The pure governance fold builder. Feed inputs in any order; call
/// [`Self::finish`] (or [`Self::run`]) to converge.
#[derive(Debug, Default)]
pub struct GovernanceFold {
    room_id: Option<crate::ids::RoomId>,
    entries: Vec<GovernanceEntryBody>,
    approvals: Vec<ApprovalBody>,
    resolutions: Vec<ForkResolutionBody>,
}

impl GovernanceFold {
    /// An empty fold.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a fold for a specific room.
    #[must_use]
    pub fn for_room(room_id: crate::ids::RoomId) -> Self {
        Self {
            room_id: Some(room_id),
            ..Self::default()
        }
    }

    /// Add a verified governance entry.
    #[must_use]
    pub fn entry(mut self, body: GovernanceEntryBody) -> Self {
        if self.room_id.is_none() {
            self.room_id = Some(body.room_id);
        }
        self.entries.push(body);
        self
    }

    /// Add a verified approval.
    #[must_use]
    pub fn approval(mut self, body: ApprovalBody) -> Self {
        self.approvals.push(body);
        self
    }

    /// Add a verified fork resolution.
    #[must_use]
    pub fn resolution(mut self, body: ForkResolutionBody) -> Self {
        self.resolutions.push(body);
        self
    }

    /// Add an iterator of entries.
    #[must_use]
    pub fn entries_from(mut self, bodies: impl IntoIterator<Item = GovernanceEntryBody>) -> Self {
        for b in bodies {
            if self.room_id.is_none() {
                self.room_id = Some(b.room_id);
            }
            self.entries.push(b);
        }
        self
    }

    /// Converge the fold and compute the final outcome.
    ///
    /// # Errors
    /// Returns [`Reject::MissingDependency`] if no entries were supplied and no
    /// room id was set.
    pub fn finish(self) -> Result<FoldOutcome, Reject> {
        let room_id = self.room_id.ok_or(Reject::MissingDependency)?;
        let mut entries_by_id: BTreeMap<GovernanceEntryId, &GovernanceEntryBody> = BTreeMap::new();
        for e in &self.entries {
            entries_by_id.insert(entry_id_of(e), e);
        }

        // Group approvals by the entry they reference.
        let mut approvals_by_entry: BTreeMap<GovernanceEntryId, Vec<&ApprovalBody>> =
            BTreeMap::new();
        for a in &self.approvals {
            approvals_by_entry.entry(a.entry_id).or_default().push(a);
        }

        // Pass 1: fold the full entry set with nothing dropped. This detects the
        // forks (evidence pairs) and establishes the immutable admin, which are
        // both required to *authorize* and *apply* fork resolutions.
        let empty_dropped: BTreeSet<GovernanceEntryId> = BTreeSet::new();
        let (mut pass1_state, _pass1_items) =
            Self::fold_pass(room_id, &entries_by_id, &approvals_by_entry, &empty_dropped);

        // Authorize + match resolutions against the detected forks (spec D6).
        // Only a resolution that (a) is for this room, (b) is signed by the
        // immutable admin, and (c) names an actual fork's evidence pair takes
        // effect. Each accepted resolution both marks its evidence resolved and
        // decides which conflicting entries to drop from the re-derived state:
        //   - Reject       → drop both conflicting entries;
        //   - Accept{win}  → drop the loser (the non-winner of the pair).
        let mut resolved_evidence: BTreeSet<[GovernanceEntryId; 2]> = BTreeSet::new();
        let mut dropped: BTreeSet<GovernanceEntryId> = BTreeSet::new();
        for res in &self.resolutions {
            if !resolution_is_authorized(&pass1_state, res) {
                continue;
            }
            // The resolution must correspond to an actual detected fork.
            let Some(ev) = pass1_state
                .forks
                .iter()
                .find(|f| f.conflicting == res.evidence)
            else {
                continue;
            };
            resolved_evidence.insert(ev.conflicting);
            match &res.action {
                ForkResolveAction::Reject => {
                    dropped.insert(ev.conflicting[0]);
                    dropped.insert(ev.conflicting[1]);
                }
                ForkResolveAction::Accept { winner } => {
                    let loser = if &ev.conflicting[0] == winner {
                        ev.conflicting[1]
                    } else {
                        ev.conflicting[0]
                    };
                    dropped.insert(loser);
                }
            }
        }

        // Pass 2: re-derive the authoritative state with the resolved-away
        // entries excluded, so the losing branch's mutations are rolled back and
        // an accepted winner is actually applied (blocker: winner-selection must
        // have a state effect). When nothing was dropped this equals pass 1.
        let (mut state, mut items) = if dropped.is_empty() {
            Self::fold_pass(room_id, &entries_by_id, &approvals_by_entry, &empty_dropped)
        } else {
            Self::fold_pass(room_id, &entries_by_id, &approvals_by_entry, &dropped)
        };

        // Mark dropped entries as rejected-by-resolution in the item log so the
        // decision is auditable even though they no longer affect state.
        for id in &dropped {
            items.insert(
                *id,
                FoldItem::Rejected {
                    id: *id,
                    reason: Reject::ForkDetected,
                },
            );
        }

        // The canonical fork list is pass 1's detected evidence, with the
        // authorized resolutions' evidence flagged resolved. Re-deriving in pass
        // 2 with entries dropped would otherwise lose the (now-resolved)
        // evidence entirely; carrying it forward keeps the fork hash-visible.
        for ev in &mut pass1_state.forks {
            if resolved_evidence.contains(&ev.conflicting) {
                ev.resolved = true;
            }
        }
        state.forks = std::mem::take(&mut pass1_state.forks);

        // Deterministic fork ordering for hashing/inspection.
        state.forks.sort();
        state.forks.dedup();

        let (member_root, _projection) = projection::project(&state);
        let state_root = crate::governance::state_root::compute(&state, &member_root);

        let unresolved_forks: Vec<_> = state
            .forks
            .iter()
            .filter(|f| !f.resolved)
            .cloned()
            .collect();
        let items_vec = items.into_values().collect();
        Ok(FoldOutcome {
            room_id,
            items: items_vec,
            unresolved_forks,
            state,
            state_root,
            member_root,
        })
    }

    /// One deterministic fixpoint pass over the entry set, excluding any id in
    /// `dropped`. Returns the folded state and the per-entry decision log. This
    /// is the arrival-order-independent core shared by both fold passes.
    fn fold_pass(
        room_id: crate::ids::RoomId,
        entries_by_id: &BTreeMap<GovernanceEntryId, &GovernanceEntryBody>,
        approvals_by_entry: &BTreeMap<GovernanceEntryId, Vec<&ApprovalBody>>,
        dropped: &BTreeSet<GovernanceEntryId>,
    ) -> (GovernanceState, BTreeMap<GovernanceEntryId, FoldItem>) {
        let mut state = GovernanceState::empty(room_id);
        let mut items: BTreeMap<GovernanceEntryId, FoldItem> = BTreeMap::new();

        let mut progress = true;
        while progress {
            progress = false;
            for (&id, entry) in entries_by_id {
                if items.contains_key(&id) || dropped.contains(&id) {
                    continue;
                }
                match Self::classify(entry, &state, id, approvals_by_entry) {
                    Class::Accept => {
                        apply_entry(&mut state, entry, id);
                        items.insert(id, FoldItem::Accepted(id));
                        progress = true;
                    }
                    Class::Reject(reason) => {
                        items.insert(id, FoldItem::Rejected { id, reason });
                        progress = true;
                    }
                    Class::Fork(evidence) => {
                        state.forks.push(evidence);
                        items.insert(
                            id,
                            FoldItem::Rejected {
                                id,
                                reason: Reject::ForkDetected,
                            },
                        );
                        progress = true;
                    }
                    Class::Wait => {}
                }
            }
        }

        // Entries still unclassified after the fixpoint are missing dependencies.
        for &id in entries_by_id.keys() {
            if dropped.contains(&id) {
                continue;
            }
            items.entry(id).or_insert(FoldItem::Rejected {
                id,
                reason: Reject::MissingDependency,
            });
        }

        (state, items)
    }

    /// Classify one entry against the running state. Pure / local to a fixpoint
    /// step.
    fn classify(
        entry: &GovernanceEntryBody,
        state: &GovernanceState,
        id: GovernanceEntryId,
        approvals_by_entry: &BTreeMap<GovernanceEntryId, Vec<&ApprovalBody>>,
    ) -> Class {
        // Room binding.
        if entry.room_id != state.room_id {
            return Class::Reject(Reject::InvalidContent);
        }

        // Genesis special-case: the very first entry must be InitRoom, seq 1, no
        // parent, and it establishes the admin. Until genesis arrives, every
        // non-genesis entry must WAIT (it may become applicable once genesis
        // lands); only the fixpoint end-of-pass turns a permanent wait into
        // MissingDependency.
        let is_genesis = matches!(entry.action, GovernanceAction::InitRoom { .. });
        if state.admin.is_none() {
            if !is_genesis {
                return Class::Wait;
            }
            if entry.seq != 1 || entry.parent.is_some() {
                return Class::Reject(Reject::InvalidContent);
            }
            // Genesis authorization: admin must equal author.
            return match authz::authorize_governance_entry(state, entry, &[]) {
                Ok(()) => Class::Accept,
                Err(r) => Class::Reject(r),
            };
        }
        // After genesis, no further InitRoom is allowed.
        if is_genesis {
            return Class::Reject(Reject::InvalidContent);
        }

        // Fork detection against the author's existing tip.
        if let Some(existing_tip) = state.author_tip(&entry.author) {
            let existing_seq = state.author_seq(&entry.author);
            if let Some(evidence) = fork::detect(
                entry.author,
                id,
                entry.seq,
                entry.parent,
                Some((existing_seq, existing_tip)),
            ) {
                return Class::Fork(evidence);
            }
        }

        // Sequence continuity: seq must be exactly prev+1 and parent must link.
        let prev_seq = state.author_seq(&entry.author);
        if entry.seq != prev_seq + 1 {
            return Class::Wait;
        }
        let expected_parent = state.author_tip(&entry.author);
        if entry.parent != expected_parent {
            return Class::Wait;
        }

        // Gather approvals for this entry.
        let owned_approvals: Vec<ApprovalBody> = approvals_by_entry
            .get(&id)
            .map(|v| v.iter().map(|a| (*a).clone()).collect())
            .unwrap_or_default();
        match authz::authorize_governance_entry(state, entry, &owned_approvals) {
            Ok(()) => Class::Accept,
            Err(r) => Class::Reject(r),
        }
    }
}

#[derive(Debug)]
enum Class {
    Accept,
    Reject(Reject),
    Fork(crate::governance::model::ForkEvidence),
    Wait,
}

/// Apply an accepted entry to the running state (mutates membership, tips,
/// policy). Deterministic: the same entry against the same state yields the same
/// next state.
fn apply_entry(state: &mut GovernanceState, entry: &GovernanceEntryBody, id: GovernanceEntryId) {
    match &entry.action {
        GovernanceAction::InitRoom {
            admin,
            admin_device,
            room_name: _,
        } => {
            state.admin = Some(*admin);
            state.members.insert(
                *admin,
                MemberRecord {
                    member_id: *admin,
                    role: Role::Admin,
                    status: MemberStatus::Active,
                    devices: vec![*admin_device],
                    governance_cursor: id,
                },
            );
        }
        GovernanceAction::AddMember {
            member,
            device,
            role,
        } => {
            state.members.insert(
                *member,
                MemberRecord {
                    member_id: *member,
                    role: *role,
                    status: MemberStatus::Active,
                    devices: vec![*device],
                    governance_cursor: id,
                },
            );
        }
        GovernanceAction::RemoveMember { member } => {
            if let Some(m) = state.members.get_mut(member) {
                m.status = MemberStatus::Removed;
                m.governance_cursor = id;
            }
        }
        GovernanceAction::SetRole { member, role } => {
            if let Some(m) = state.members.get_mut(member) {
                m.role = *role;
                m.governance_cursor = id;
            }
        }
        GovernanceAction::RotateDevice { member, device } => {
            if let Some(m) = state.members.get_mut(member) {
                if !m.devices.contains(device) {
                    m.devices.push(*device);
                }
                m.governance_cursor = id;
            }
        }
        GovernanceAction::SetPolicy { policy } => {
            state.policy = policy.clone();
        }
    }
    // Advance the author's tip.
    state.tips.insert(entry.author, (entry.seq, id));
}

/// Whether a fork resolution is authorized to mutate state (spec D6 / #149).
///
/// A resolution takes effect only when it is bound to the room and authorized by
/// the governance layer — i.e. signed by the immutable admin. This is the gate
/// the self-review found missing: previously any resolution (even one authored by
/// nobody) cleared matching evidence. The `signer` field is bound to the verified
/// envelope signer at [`fork::decode_verified`]; here we check that signer is the
/// admin via [`authz::authorize_fork_resolution`].
fn resolution_is_authorized(state: &GovernanceState, res: &ForkResolutionBody) -> bool {
    if res.room_id != state.room_id {
        return false;
    }
    authz::authorize_fork_resolution(state, &res.signer).is_ok()
}

/// Compute the deterministic id of an entry body without re-signing.
fn entry_id_of(body: &GovernanceEntryBody) -> GovernanceEntryId {
    let csb = crate::signed::to_csb(body);
    GovernanceEntryId::from_bytes(crate::domain::blake3_domain(
        crate::domain::GOVERNANCE_ENTRY_ID,
        &csb,
    ))
}

/// Build a single entry id from its body (re-exported for callers/tests).
#[must_use]
pub fn entry_id(body: &GovernanceEntryBody) -> GovernanceEntryId {
    entry_id_of(body)
}

/// Build a single approval id from its body.
#[must_use]
pub fn approval_id(body: &ApprovalBody) -> crate::ids::ApprovalId {
    let csb = crate::signed::to_csb(body);
    crate::ids::ApprovalId::from_bytes(crate::domain::blake3_domain(
        crate::domain::GOVERNANCE_APPROVAL_ID,
        &csb,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DeviceId, MemberId, RoomId, LEN};
    use crate::keys::SigningKey;

    fn admin_key() -> SigningKey {
        SigningKey::from_seed(&[0xa0; LEN])
    }

    fn genesis(room: RoomId, key: &SigningKey) -> GovernanceEntryBody {
        GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: key.member_id(),
            seq: 1,
            parent: None,
            epoch: 1_000,
            action: GovernanceAction::InitRoom {
                admin: key.member_id(),
                admin_device: key.device_id(),
                room_name: "room".to_owned(),
            },
        }
    }

    fn add_member(
        room: RoomId,
        author: &SigningKey,
        seq: u64,
        parent: GovernanceEntryId,
        member: MemberId,
    ) -> GovernanceEntryBody {
        GovernanceEntryBody {
            schema_version: 2,
            room_id: room,
            author: author.member_id(),
            seq,
            parent: Some(parent),
            epoch: 1_001,
            action: GovernanceAction::AddMember {
                member,
                device: DeviceId::from_bytes([0; LEN]),
                role: Role::Member,
            },
        }
    }

    #[test]
    fn genesis_only_fold_has_admin_and_stable_root() {
        let room = RoomId::from_bytes([0x70; LEN]);
        let key = admin_key();
        let g = genesis(room, &key);
        let outcome = GovernanceFold::new().entry(g).finish().unwrap();
        assert_eq!(outcome.state.admin, Some(key.member_id()));
        // Re-folding the same set yields the same root (determinism).
        let g2 = genesis(room, &key);
        let outcome2 = GovernanceFold::new().entry(g2).finish().unwrap();
        assert_eq!(outcome.state_root, outcome2.state_root);
    }

    #[test]
    fn fold_is_arrival_order_independent() {
        let room = RoomId::from_bytes([0x70; LEN]);
        let admin = admin_key();
        let g = genesis(room, &admin);
        let gid = entry_id(&g);
        let bob = MemberId::from_bytes([0xb0; LEN]);
        let add = add_member(room, &admin, 2, gid, bob);

        // Order A: genesis then add.
        let a = GovernanceFold::new()
            .entry(g.clone())
            .entry(add.clone())
            .finish()
            .unwrap();
        // Order B: add then genesis (out of order).
        let b = GovernanceFold::new()
            .entry(add.clone())
            .entry(g.clone())
            .finish()
            .unwrap();
        assert_eq!(a.state_root, b.state_root, "arrival order must not matter");
        assert!(a.state.is_active(&bob));
        assert!(b.state.is_active(&bob));
    }

    #[test]
    fn non_admin_add_member_rejected() {
        let room = RoomId::from_bytes([0x70; LEN]);
        let admin = admin_key();
        let g = genesis(room, &admin);
        let intruder = SigningKey::from_seed(&[0xee; LEN]);
        // The intruder's first entry starts its own per-author chain: seq 1,
        // parent None. Authorization rejects it (intruder is not the admin).
        let mut bad = add_member(
            room,
            &intruder,
            1,
            GovernanceEntryId::from_bytes([0; LEN]),
            MemberId::from_bytes([0xc0; LEN]),
        );
        bad.parent = None;
        let outcome = GovernanceFold::new().entry(g).entry(bad).finish().unwrap();
        assert!(outcome.items.iter().any(|i| matches!(
            i,
            FoldItem::Rejected {
                reason: Reject::InsufficientAuthorization,
                ..
            }
        )));
    }

    #[test]
    fn same_seq_fork_detected() {
        let room = RoomId::from_bytes([0x70; LEN]);
        let admin = admin_key();
        let g = genesis(room, &admin);
        let gid = entry_id(&g);
        // Two entries from admin both at seq 2 with the same parent → fork.
        let a = add_member(room, &admin, 2, gid, MemberId::from_bytes([0x01; LEN]));
        let b = add_member(room, &admin, 2, gid, MemberId::from_bytes([0x02; LEN]));
        let outcome = GovernanceFold::new()
            .entry(g)
            .entry(a)
            .entry(b)
            .finish()
            .unwrap();
        assert!(
            !outcome.unresolved_forks.is_empty(),
            "a same-seq fork must be detected"
        );
    }

    #[test]
    fn empty_fold_is_missing_dependency() {
        assert_eq!(
            GovernanceFold::new().finish().err(),
            Some(Reject::MissingDependency)
        );
    }
}
