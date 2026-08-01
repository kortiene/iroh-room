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

use std::collections::BTreeMap;

use iroh_rooms_crypto::RoomKey;

/// Per-epoch key state: a held key, or a poisoned epoch that adopts nothing.
enum EpochKeyState {
    /// The single key accepted for this epoch.
    Key(RoomKey),
    /// Conflicting keys were offered (spec D5a): the epoch fails closed and
    /// holds no key until a (step 6) deterministic resolution.
    Poisoned,
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
}

impl EpochKeyStore {
    /// Offer `key` for `epoch`. Idempotent for identical bytes; a different
    /// key for a held epoch — or any key for an already-poisoned epoch —
    /// poisons the epoch and returns the conflict (spec D5a: adopt neither).
    pub fn insert(&mut self, epoch: u64, key: RoomKey) -> Result<(), EpochKeyConflict> {
        match self.epochs.get(&epoch) {
            None => {
                self.epochs.insert(epoch, EpochKeyState::Key(key));
                Ok(())
            }
            Some(EpochKeyState::Key(held)) if held.as_bytes() == key.as_bytes() => Ok(()),
            Some(_) => {
                // Conflicting offer or already poisoned: drop whatever was
                // held (RoomKey zeroizes) and pin the epoch poisoned.
                self.epochs.insert(epoch, EpochKeyState::Poisoned);
                Err(EpochKeyConflict { epoch })
            }
        }
    }

    /// The held key for `epoch`; `None` when absent **or** poisoned.
    pub fn get(&self, epoch: u64) -> Option<&RoomKey> {
        match self.epochs.get(&epoch) {
            Some(EpochKeyState::Key(k)) => Some(k),
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
}

#[cfg(test)]
mod tests {
    use super::{EpochKeyConflict, EpochKeyStore};
    use iroh_rooms_crypto::RoomKey;

    fn key(byte: u8) -> RoomKey {
        RoomKey::from_bytes([byte; 32])
    }

    #[test]
    fn insert_then_get_round_trips() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA)).expect("fresh insert");
        assert!(store.has(3));
        assert_eq!(store.get(3).expect("held").as_bytes(), &[0xAA; 32]);
        assert!(!store.has(4));
        assert!(!store.is_poisoned(3));
    }

    #[test]
    fn same_bytes_reinsert_is_idempotent() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA)).expect("fresh insert");
        store.insert(3, key(0xAA)).expect("same bytes are a no-op");
        assert!(store.has(3));
    }

    #[test]
    fn conflicting_key_poisons_the_epoch_and_adopts_neither() {
        let mut store = EpochKeyStore::default();
        store.insert(3, key(0xAA)).expect("fresh insert");
        assert_eq!(
            store.insert(3, key(0xBB)).expect_err("conflict"),
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
        store.insert(3, key(0xAA)).expect("fresh insert");
        store.insert(3, key(0xBB)).expect_err("poisons");
        // Even re-offering one of the original candidates stays refused: the
        // conflict is resolved by the step-6 deterministic rule, not arrival.
        store.insert(3, key(0xAA)).expect_err("stays poisoned");
        store.insert(3, key(0xBB)).expect_err("stays poisoned");
        assert!(store.is_poisoned(3));
        // Other epochs are unaffected.
        store.insert(4, key(0xCC)).expect("independent epoch");
        assert!(store.has(4));
    }
}
