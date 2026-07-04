//! A stderr-rendering [`AuditSink`] for the `room`/`join` network commands (spec
//! IR-0110 §5.4/§5.9; project memory *CLI has no tracing subscriber*).
//!
//! The CLI installs no `tracing` subscriber, so the default [`TracingAudit`]
//! output is silently dropped. [`StderrAudit`] renders the security-relevant
//! events directly on stderr instead: an advisory flag on an accepted event
//! (`warning[clock_skew]: …`, never a failure) and a coarse rejected-frame note. The
//! fine-grained per-code reject distinction AC1 needs (`bad_signature` vs
//! `not_a_member`) is rendered by [`crate::message::tail`] polling
//! [`Node::logs`](iroh_rooms_net::Node::logs) — see that module — so this sink
//! stays a thin, always-available fallback for every command, not only `room
//! tail`. stdout is never touched here (script-friendly output stays clean).

use iroh_rooms::experimental::session::EndpointId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_net::{AuditSink, BlobDenyCause, RejectCause};

use crate::message::{short_device, short_hash};

/// Renders [`AuditSink`] callbacks as stable, greppable stderr lines.
#[derive(Debug, Clone, Copy, Default)]
pub struct StderrAudit;

impl AuditSink for StderrAudit {
    fn accepted(&self, _device: EndpointId, _identity: &IdentityKey) {
        // Chatter; the connection panel (`room members --status` / `room tail`)
        // already renders the live roster.
    }

    fn rejected(&self, device: EndpointId, cause: RejectCause) {
        eprintln!(
            "warning[{}]: rejected a connection from {device} before accepting any event bytes",
            cause.code()
        );
    }

    fn connected(&self, _device: EndpointId) {}

    fn disconnected(&self, _device: EndpointId) {}

    fn offline(&self, device: EndpointId, reason: &'static str) {
        eprintln!("note: peer {device} went offline (reason={reason})");
    }

    fn event_rejected(&self, device: EndpointId, count: u64) {
        // A coarse, always-on fallback. The per-code distinction AC1 requires
        // (`bad_signature` vs `not_a_member`) is rendered by `room tail`'s
        // `Node::logs()` poll (spec §5.8/OQ-2); not every command polls that, so
        // this note stays the baseline signal for the rest.
        eprintln!(
            "note: dropped {count} invalid inbound event(s) from {device}; not stored, not \
             re-broadcast"
        );
    }

    fn deauthorized(&self, device: EndpointId) {
        eprintln!("note: peer {device} was removed from the room mid-session");
    }

    fn event_flagged(&self, device: EndpointId, code: &'static str) {
        // The pinned advisory-render contract (spec §5.2): never an error, never a
        // non-zero exit.
        eprintln!(
            "warning[{code}]: an inbound event from {device} was flagged ({code}); accepted, \
             not rejected"
        );
    }

    fn blob_serve_accepted(&self, peer: EndpointId, hash: [u8; 32]) {
        // `blob.serve.accepted` is the stable, greppable audit line (IR-0204 §7).
        eprintln!(
            "blob.serve.accepted peer={} hash={}",
            short_device(&peer),
            short_hash(hash)
        );
    }

    fn blob_serve_rejected(&self, peer: EndpointId, cause: BlobDenyCause, hash: Option<[u8; 32]>) {
        eprintln!(
            "blob.serve.rejected:{} peer={}{}",
            cause.code(),
            short_device(&peer),
            hash.map_or_else(String::new, |h| format!(" hash={}", short_hash(h)))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::StderrAudit;
    use iroh_rooms::experimental::session::{EndpointId, SecretKey};
    use iroh_rooms_core::event::keys::IdentityKey;
    use iroh_rooms_net::{AuditSink, BlobDenyCause, RejectCause};

    use crate::message::{short_device, short_hash};

    fn device(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn every_hook_is_callable_without_panicking() {
        let sink = StderrAudit;
        let id = IdentityKey::from_bytes([0x11; 32]);
        sink.accepted(device(1), &id);
        sink.rejected(device(1), RejectCause::UnknownDevice);
        sink.connected(device(1));
        sink.disconnected(device(1));
        sink.offline(device(1), "unreachable");
        sink.event_rejected(device(1), 3);
        sink.deauthorized(device(1));
        sink.event_flagged(device(1), "clock_skew");
        sink.blob_serve_accepted(device(1), [0x22; 32]);
        sink.blob_serve_rejected(device(1), BlobDenyCause::NotActive, Some([0x22; 32]));
        sink.blob_serve_rejected(device(1), BlobDenyCause::PushDenied, None);
    }

    #[test]
    fn blob_serve_lines_key_on_the_same_short_device_and_hash_as_message_tail() {
        let hash = [0x33; 32];
        assert_eq!(short_hash(hash), "33333333");
        assert_eq!(short_device(&device(1)), device(1).to_string()[..8]);

        let sink = StderrAudit;
        sink.blob_serve_accepted(device(1), hash);
        sink.blob_serve_rejected(device(1), BlobDenyCause::NotReferenced, Some(hash));
    }
}
