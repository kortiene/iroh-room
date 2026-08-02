//! The engine's in-memory `epoch → room_key` store (#191 step 4, spec D5a/D7).
//!
//! Session-only on purpose: persistence, the T28 at-rest posture, and
//! fold-driven key adoption from `MemberKeyDistribution` payloads are the
//! rotation lifecycle (spec §7 step 6). What is **not** deferred is the D5a
//! conflict rule, because getting it wrong here would hand step 6 an
//! arrival-order-dependent substrate: offering a *different* key for an epoch
//! that already holds one poisons the epoch — the store adopts **neither**
//! key (the held one is dropped and zeroized), reads for that epoch fail
//! closed, and further inserts are refused. Re-offering the *same* bytes is
//! an idempotent no-op. Un-poisoning is the step-6 deterministic
//! fork-resolution rule's job; no clear API exists yet by design.

use std::collections::{BTreeMap, BTreeSet};

use iroh_rooms_crypto::RoomKey;

use crate::event::ids::EventId;

/// Per-epoch key state: a held key, a poisoned epoch awaiting deterministic
/// resolution, or a resolved poisoned epoch that now holds the winning key.
enum EpochKeyState {
    /// The single key accepted for this epoch, plus the event that offered it.
    Key(RoomKey, EventId),
    /// Conflicting keys were offered (spec D5a): the epoch fails closed and
    /// holds no key until the step-6 deterministic resolution.
    Poisoned,
}

/// A same-epoch key conflict candidate: a distribution event that offered a
/// key for this epoch. Kept only while the epoch is poisoned so a deterministic
/// resolution can pick the winner.
#[derive(Debug, Clone)]
pub(crate) struct ConflictCandidate {
    /// The distribution event that offered this key.
    pub event_id: EventId,
    /// The offered key, if this device was able to unwrap it. `None` when the
    /// candidate is retained for commitment-only conflict detection (the local
    /// device is not in the wrapped set).
    pub key: Option<RoomKey>,
}

/// A same-epoch key conflict (spec D5a): the epoch is now poisoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EpochKeyConflict {
    /// The poisoned epoch.
    pub epoch: u64,
}

/// In-memory epoch key store. `RoomKey` zeroizes on drop, so dropped or
/// replaced state never leaves key bytes behind.
#[derive(Default)]
pub(crate) struct EpochKeyStore {
    epochs: BTreeMap<u64, EpochKeyState>,
    /// Conflict candidates retained only for poisoned epochs. Dropped when the
    /// epoch is resolved or when the store is dropped.
    candidates: BTreeMap<u64, Vec<ConflictCandidate>>,
}

impl EpochKeyStore {
    /// Offer `key` for `epoch`, attributed to `event_id`. Idempotent for
    /// identical bytes; a different key for a held epoch — or any key for an
    /// already-poisoned epoch — poisons the epoch and returns the conflict
    /// (spec D5a: adopt neither). The previously-held key is retained as a
    /// conflict candidate so deterministic resolution can compare event ids.
    /// The caller may later add the new conflicting candidate and call `resolve`.
    pub fn insert(
        &mut self,
        epoch: u64,
        key: RoomKey,
        event_id: EventId,
    ) -> Result<(), EpochKeyConflict> {
        match self.epochs.get(&epoch) {
            None => {
                self.epochs.insert(epoch, EpochKeyState::Key(key, event_id));
                Ok(())
            }
            Some(EpochKeyState::Key(held, _)) if held.as_bytes() == key.as_bytes() => Ok(()),
            Some(EpochKeyState::Key(held, held_id)) => {
                // Conflicting offer: retain the previously-held key as a
                // candidate, then poison the epoch.
                let held_candidate = ConflictCandidate {
                    event_id: *held_id,
                    key: Some(held.clone()),
                };
                self.epochs.insert(epoch, EpochKeyState::Poisoned);
                self.candidates
                    .entry(epoch)
                    .or_default()
                    .push(held_candidate);
                Err(EpochKeyConflict { epoch })
            }
            Some(EpochKeyState::Poisoned) => Err(EpochKeyConflict { epoch }),
        }
    }

    /// Record a conflict candidate for a poisoned `epoch`. Silently ignored if
    /// the epoch is not currently poisoned (the candidate is irrelevant once
    /// resolved or if no conflict ever occurred).
    pub fn add_conflict_candidate(&mut self, epoch: u64, candidate: ConflictCandidate) {
        if self.is_poisoned(epoch) {
            self.candidates.entry(epoch).or_default().push(candidate);
        }
    }

    /// Poison `epoch` directly (spec D5a), used when a fold-visible commitment
    /// conflict is detected independently of unwrap ability. The held key, if
    /// any, is dropped and the epoch fails closed until deterministic resolution.
    pub fn poison(&mut self, epoch: u64) {
        self.epochs.insert(epoch, EpochKeyState::Poisoned);
    }

    /// Deterministically resolve a poisoned epoch (spec D5a step-6 rule). Among
    /// all retained candidates, the candidate whose `event_id` is
    /// lexicographically smallest wins and its key becomes the held key. If no
    /// candidates are recorded, the epoch stays poisoned.
    ///
    /// The deterministic rule is convergent: every node that has seen the same
    /// set of conflicting distributions selects the same winner because event
    /// ids are uniformly-distributed hashes and the candidate set is bounded by
    /// the distributions the node has observed.
    pub fn resolve(&mut self, epoch: u64) -> bool {
        if !self.is_poisoned(epoch) {
            return false;
        }
        let Some(list) = self.candidates.remove(&epoch) else {
            return false;
        };
        // Only candidates whose key this device could unwrap are eligible to
        // win; commitment-only candidates cannot become the held key.
        let winner = list
            .into_iter()
            .filter(|c| c.key.is_some())
            .min_by(|a, b| a.event_id.as_bytes().cmp(b.event_id.as_bytes()));
        if let Some(winner) = winner {
            let key = winner.key.expect("filtered to Some");
            self.epochs
                .insert(epoch, EpochKeyState::Key(key, winner.event_id));
            true
        } else {
            false
        }
    }

    /// The held key for `epoch`; `None` when absent **or** poisoned.
    pub fn get(&self, epoch: u64) -> Option<&RoomKey> {
        match self.epochs.get(&epoch) {
            Some(EpochKeyState::Key(k, _)) => Some(k),
            _ => None,
        }
    }

    /// The held key for `epoch` together with the event that offered it;
    /// `None` when absent **or** poisoned. Used to persist the outcome of a
    /// deterministic D5a resolution with its winning `source_event_id`.
    pub fn get_with_source(&self, epoch: u64) -> Option<(&RoomKey, EventId)> {
        match self.epochs.get(&epoch) {
            Some(EpochKeyState::Key(k, id)) => Some((k, *id)),
            _ => None,
        }
    }

    /// Whether a usable key is held for `epoch` (poisoned ⇒ `false`).
    pub fn has(&self, epoch: u64) -> bool {
        self.get(epoch).is_some()
    }

    /// Whether `epoch` is poisoned by a D5a conflict.
    pub fn is_poisoned(&self, epoch: u64) -> bool {
        matches!(self.epochs.get(&epoch), Some(EpochKeyState::Poisoned))
    }

    /// Every epoch that currently holds a usable key, in ascending order.
    #[allow(dead_code)] // used by the key-history serve path (step 7)
    pub fn held_epochs(&self) -> Vec<u64> {
        self.epochs
            .iter()
            .filter_map(|(epoch, state)| {
                matches!(state, EpochKeyState::Key(_, _)).then_some(*epoch)
            })
            .collect()
    }

    /// Iterate held `(epoch, key)` pairs in ascending epoch order.
    pub fn iter_held(&self) -> impl Iterator<Item = (u64, &RoomKey)> {
        self.epochs.iter().filter_map(|(epoch, state)| match state {
            EpochKeyState::Key(k, _) => Some((*epoch, k)),
            EpochKeyState::Poisoned => None,
        })
    }

    /// Poisoned epochs that still have no deterministic resolution.
    #[allow(dead_code)] // used by the key-history serve path (step 7)
    pub fn poisoned_epochs(&self) -> BTreeSet<u64> {
        self.epochs
            .iter()
            .filter_map(|(epoch, state)| matches!(state, EpochKeyState::Poisoned).then_some(*epoch))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConflictCandidate, EpochKeyConflict, EpochKeyStore};
    use crate::event::ids::EventId;
    use iroh_rooms_crypto::RoomKey;

    fn key(byte: u8) -> RoomKey {
        RoomKey::from_bytes([byte; 32])
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_bytes([byte; 32])
    }

    #[test]
    fn insert_then_get_round_trips() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA), eid(0x01)).expect("fresh insert");
        assert!(store.has(3));
        assert_eq!(store.get(3).expect("held").as_bytes(), &[0xAA; 32]);
        assert!(!store.has(4));
        assert!(!store.is_poisoned(3));
    }

    #[test]
    fn same_bytes_reinsert_is_idempotent() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA), eid(0x01)).expect("fresh insert");
        store
            .insert(3, key(0xAA), eid(0x01))
            .expect("same bytes are a no-op");
        assert!(store.has(3));
    }

    #[test]
    fn conflicting_key_poisons_the_epoch_and_adopts_neither() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA), eid(0x01)).expect("fresh insert");
        assert_eq!(
            store.insert(3, key(0xBB), eid(0x02)).expect_err("conflict"),
            EpochKeyConflict { epoch: 3 }
        );
        // D5a: neither key is usable — not even the first-arrived one.
        assert!(!store.has(3));
        assert!(store.get(3).is_none());
        assert!(store.is_poisoned(3));
    }

    #[test]
    fn poisoned_epoch_refuses_every_later_offer() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA), eid(0x01)).expect("fresh insert");
        store.insert(3, key(0xBB), eid(0x02)).expect_err("poisons");
        // Even re-offering one of the original candidates stays refused: the
        // conflict is resolved by the step-6 deterministic rule, not arrival.
        store
            .insert(3, key(0xAA), eid(0x03))
            .expect_err("stays poisoned");
        store
            .insert(3, key(0xBB), eid(0x04))
            .expect_err("stays poisoned");
        assert!(store.is_poisoned(3));
        // Other epochs are unaffected.
        store
            .insert(4, key(0xCC), eid(0x05))
            .expect("independent epoch");
        assert!(store.has(4));
    }

    #[test]
    fn deterministic_resolution_picks_smallest_event_id() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA), eid(0x01)).expect("fresh insert");
        store.insert(3, key(0xBB), eid(0x02)).expect_err("poisons");
        // The original held key was automatically retained as a candidate
        // (eid 0x01). Add a third candidate with an even smaller event id.
        store.add_conflict_candidate(
            3,
            ConflictCandidate {
                event_id: EventId::from_bytes([0x00; 32]),
                key: Some(key(0xCC)),
            },
        );
        assert!(store.resolve(3));
        assert!(store.has(3));
        // Winner is the candidate with event_id [0x00;32], whose key is 0xCC.
        assert_eq!(store.get(3).expect("held").as_bytes(), &[0xCC; 32]);
    }

    #[test]
    fn resolve_with_only_retained_original_leaves_epoch_holding_original() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA), eid(0x01)).expect("fresh insert");
        store.insert(3, key(0xBB), eid(0x02)).expect_err("poisons");
        // The original held key was automatically retained; resolving with no
        // extra candidates restores it.
        assert!(store.resolve(3));
        assert!(store.has(3));
        assert_eq!(store.get(3).expect("held").as_bytes(), &[0xAA; 32]);
    }
}
