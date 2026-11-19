# iroh-rooms-core

Deterministic room-event protocol: signing, validation, the membership fold, invite tickets, and the optional local store and sync engine.

Applications should depend on the `iroh-rooms` façade. This crate is published because that façade re-exports these types. Versioning, ownership, and publish order are in [`docs/sdk-publish.md`](../../docs/sdk-publish.md).
