//! **Experimental (unstable API).** The local `SQLite` event store: idempotent
//! persistence of validated events, derived query indexes, and a
//! deterministic rebuild.

pub use iroh_rooms_core::store::{
    EventStore, InsertOutcome, InsertStats, ParkedRow, StoreError, StoreOptions, StoredEvent,
    SyncStateRow, TrustRow,
};
