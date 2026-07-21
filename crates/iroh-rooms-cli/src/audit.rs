//! CLI-local [`AuditSink`] implementations for the `room`/`join`/`file` network
//! commands (spec IR-0110 §5.4/§5.9; project memory *CLI has no tracing
//! subscriber*).
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
//!
//! [`PersistentAudit`] appends the same class of security/lifecycle signals to
//! `<IROH_ROOMS_HOME>/audit.ndjson`, giving operators a local post-run trail even
//! when the terminal output is gone. It records public identifiers and minimized
//! hashes only; it must never write capability secrets, tickets, identity secrets,
//! blob bytes, or local filesystem paths.

use std::fs::{File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use iroh_rooms::experimental::session::EndpointId;
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::membership::ACTIVE_MEMBER_WARNING_THRESHOLD;
use iroh_rooms_net::{AuditSink, BlobDenyCause, RejectCause};
use serde_json::{json, Value};

use crate::clock;
use crate::message::{short_device, short_hash};
use crate::paths;

/// Append-only audit log under the CLI data directory.
pub(crate) const AUDIT_LOG_FILE: &str = "audit.ndjson";

/// Build the default CLI audit sink: stderr for immediate operator feedback plus
/// a persistent local NDJSON audit trail.
pub(crate) fn sink(home: &Path) -> Result<Arc<dyn AuditSink>> {
    let persistent = PersistentAudit::open(home)?;
    Ok(sink_with(persistent))
}

/// Build a network audit sink sharing an already-open persistent writer.
pub(crate) fn sink_with(persistent: PersistentAudit) -> Arc<dyn AuditSink> {
    Arc::new(LocalAudit::new(persistent))
}

/// Best-effort, append-only JSON-lines audit writer.
///
/// The sink is intentionally small and local: commands already operate inside a
/// user-owned data directory, and audit volume is low. Each line is flushed after
/// write so a normally exiting CLI command leaves a readable trail without adding
/// heavyweight logging infrastructure.
#[derive(Debug, Clone)]
pub(crate) struct PersistentAudit {
    path: Arc<PathBuf>,
    file: Arc<Mutex<File>>,
    warned_write_failure: Arc<AtomicBool>,
}

impl PersistentAudit {
    pub(crate) fn open(home: &Path) -> Result<Self> {
        paths::ensure_dir(home)?;
        let path = home.join(AUDIT_LOG_FILE);
        let file = open_audit_file(&path)?;
        Ok(Self {
            path: Arc::new(path),
            file: Arc::new(Mutex::new(file)),
            warned_write_failure: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn record(&self, event: &'static str, fields: Value) {
        if let Err(err) = self.try_record(event, fields) {
            if !self.warned_write_failure.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "warning[audit_write_failed]: could not append to {}: {err:#}",
                    self.path.display()
                );
            }
        }
    }

    fn try_record(&self, event: &'static str, fields: Value) -> Result<()> {
        let mut record = serde_json::Map::new();
        record.insert("ts_ms".to_owned(), json!(clock::now_ms()));
        record.insert("event".to_owned(), json!(event));
        match fields {
            Value::Object(fields) => record.extend(fields),
            other => {
                record.insert("fields".to_owned(), other);
            }
        }

        let mut file = self
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("audit writer lock poisoned"))?;
        serde_json::to_writer(&mut *file, &Value::Object(record))
            .context("could not encode audit record")?;
        file.write_all(b"\n")
            .context("could not write audit record newline")?;
        file.flush().context("could not flush audit record")?;
        Ok(())
    }
}

fn open_audit_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("could not open audit log at {}", path.display()))?;
    tighten_audit_file_permissions(&file, path)?;
    Ok(file)
}

#[cfg(unix)]
fn tighten_audit_file_permissions(file: &File, path: &Path) -> Result<()> {
    let perms = std::fs::Permissions::from_mode(0o600);
    file.set_permissions(perms)
        .with_context(|| format!("could not set 0600 permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn tighten_audit_file_permissions(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct LocalAudit {
    stderr: StderrAudit,
    persistent: PersistentAudit,
}

impl LocalAudit {
    fn new(persistent: PersistentAudit) -> Self {
        Self {
            stderr: StderrAudit,
            persistent,
        }
    }

    fn record_peer(&self, event: &'static str, device: EndpointId) {
        self.persistent
            .record(event, json!({ "peer": device.to_string() }));
    }
}

/// Renders [`AuditSink`] callbacks as stable, greppable stderr lines.
#[derive(Debug, Clone, Copy, Default)]
pub struct StderrAudit;

impl AuditSink for LocalAudit {
    fn accepted(&self, device: EndpointId, identity: &IdentityKey) {
        self.persistent.record(
            "peer.accepted",
            json!({
                "peer": device.to_string(),
                "identity": identity.to_string(),
            }),
        );
        self.stderr.accepted(device, identity);
    }

    fn rejected(&self, device: EndpointId, cause: RejectCause) {
        self.persistent.record(
            "peer.rejected",
            json!({
                "peer": device.to_string(),
                "cause": cause.code(),
            }),
        );
        self.stderr.rejected(device, cause);
    }

    fn connected(&self, device: EndpointId) {
        self.record_peer("peer.connected", device);
        self.stderr.connected(device);
    }

    fn disconnected(&self, device: EndpointId) {
        self.record_peer("peer.disconnected", device);
        self.stderr.disconnected(device);
    }

    fn offline(&self, device: EndpointId, reason: &'static str) {
        self.persistent.record(
            "peer.offline",
            json!({
                "peer": device.to_string(),
                "reason": reason,
            }),
        );
        self.stderr.offline(device, reason);
    }

    fn event_rejected(&self, device: EndpointId, count: u64) {
        self.persistent.record(
            "event.rejected",
            json!({
                "peer": device.to_string(),
                "count": count,
            }),
        );
        self.stderr.event_rejected(device, count);
    }

    fn deauthorized(&self, device: EndpointId) {
        self.record_peer("peer.deauthorized", device);
        self.stderr.deauthorized(device);
    }

    fn event_flagged(&self, device: EndpointId, code: &'static str) {
        self.persistent.record(
            "event.flagged",
            json!({
                "peer": device.to_string(),
                "code": code,
            }),
        );
        self.stderr.event_flagged(device, code);
    }

    fn transport_queue_saturated(&self, device: EndpointId, queue: &'static str) {
        self.persistent.record(
            "transport.queue.saturated",
            json!({
                "peer": device.to_string(),
                "queue": queue,
            }),
        );
        self.stderr.transport_queue_saturated(device, queue);
    }

    fn bootstrap_admitted(&self, device: EndpointId) {
        self.record_peer("join.bootstrap.admitted", device);
        self.stderr.bootstrap_admitted(device);
    }

    fn bootstrap_upgraded(&self, device: EndpointId, identity: &IdentityKey) {
        self.persistent.record(
            "join.bootstrap.upgraded",
            json!({
                "peer": device.to_string(),
                "identity": identity.to_string(),
            }),
        );
        self.stderr.bootstrap_upgraded(device, identity);
    }

    fn bootstrap_blocked(&self, device: EndpointId, kind: &'static str) {
        self.persistent.record(
            "join.bootstrap.blocked",
            json!({
                "peer": device.to_string(),
                "kind": kind,
            }),
        );
        self.stderr.bootstrap_blocked(device, kind);
    }

    fn bootstrap_capability_proven(&self, device: EndpointId) {
        self.record_peer("join.bootstrap.capability_proven", device);
        self.stderr.bootstrap_capability_proven(device);
    }

    fn bootstrap_capability_rejected(&self, device: EndpointId) {
        self.record_peer("join.bootstrap.capability_rejected", device);
        self.stderr.bootstrap_capability_rejected(device);
    }

    fn blob_serve_accepted(&self, peer: EndpointId, hash: [u8; 32]) {
        self.persistent.record(
            "blob.serve.accepted",
            json!({
                "peer": peer.to_string(),
                "hash_prefix": short_hash(hash),
            }),
        );
        self.stderr.blob_serve_accepted(peer, hash);
    }

    fn blob_serve_rejected(&self, peer: EndpointId, cause: BlobDenyCause, hash: Option<[u8; 32]>) {
        self.persistent.record(
            "blob.serve.rejected",
            json!({
                "peer": peer.to_string(),
                "cause": cause.code(),
                "hash_prefix": hash.map(short_hash),
            }),
        );
        self.stderr.blob_serve_rejected(peer, cause, hash);
    }

    fn active_member_threshold_reached(
        &self,
        room_id: &RoomId,
        active: usize,
        max: usize,
        remaining: usize,
    ) {
        // `room.active_members.near_cap` (issue #144): operational metadata only
        // — room id + numeric counts. No secrets, tickets, bodies, blob bytes,
        // or local paths may ever be added here (ADR-0003).
        self.persistent.record(
            "room.active_members.near_cap",
            json!({
                "room": room_id.to_string(),
                "active": active,
                "max": max,
                "remaining": remaining,
                "threshold": ACTIVE_MEMBER_WARNING_THRESHOLD,
            }),
        );
        self.stderr
            .active_member_threshold_reached(room_id, active, max, remaining);
    }
}

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

    fn transport_queue_saturated(&self, device: EndpointId, queue: &'static str) {
        eprintln!("warning[transport_queue_saturated]: {queue} queue saturated for peer {device}");
    }

    fn event_flagged(&self, device: EndpointId, code: &'static str) {
        // The pinned advisory-render contract (spec §5.2): never an error, never a
        // non-zero exit.
        eprintln!(
            "warning[{code}]: an inbound event from {device} was flagged ({code}); accepted, \
             not rejected"
        );
    }

    fn bootstrap_capability_proven(&self, device: EndpointId) {
        // An expected step of a genuine join handshake (issue #112): informational,
        // like the tracing sink's INFO line.
        eprintln!("note: peer {device} proved invite possession; serving the membership closure");
    }

    fn bootstrap_capability_rejected(&self, device: EndpointId) {
        // Someone probed the join window with a bad or replayed invite secret —
        // exactly the event the local audit trail exists for (issue #122). Same
        // `warning[<code>]:` shape as the connection-reject line.
        eprintln!(
            "warning[capability_rejected]: peer {device} presented an invalid capability \
             proof; staying gated"
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

    fn active_member_threshold_reached(
        &self,
        _room_id: &RoomId,
        active: usize,
        max: usize,
        remaining: usize,
    ) {
        // Stable, greppable operator line (issue #144, spec §4 D4). Mirrors the
        // `room members --status` headroom wording so an operator scanning stderr
        // sees the same vocabulary. The room id is intentionally omitted from the
        // stderr line — it is already on the persistent record, and a `room tail`
        // session is scoped to a single room.
        let slot = if remaining == 1 { "slot" } else { "slots" };
        eprintln!(
            "warning[room_near_capacity]: room has {active}/{max} active members ({remaining} \
             {slot} remaining)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalAudit, PersistentAudit, StderrAudit, AUDIT_LOG_FILE};
    use iroh_rooms::experimental::session::{EndpointId, SecretKey};
    use iroh_rooms_core::event::keys::IdentityKey;
    use iroh_rooms_net::{AuditSink, BlobDenyCause, RejectCause};
    use serde_json::{json, Value};
    use tempfile::tempdir;

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
        sink.bootstrap_capability_proven(device(1));
        sink.bootstrap_capability_rejected(device(1));
        sink.blob_serve_accepted(device(1), [0x22; 32]);
        sink.blob_serve_rejected(device(1), BlobDenyCause::NotActive, Some([0x22; 32]));
        sink.blob_serve_rejected(device(1), BlobDenyCause::PushDenied, None);
        sink.active_member_threshold_reached(
            &iroh_rooms_core::event::ids::RoomId::from_bytes([0x33; 32]),
            4,
            5,
            1,
        );
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

    #[test]
    fn persistent_audit_appends_valid_ndjson() {
        let home = tempdir().unwrap();
        let persistent = PersistentAudit::open(home.path()).unwrap();

        persistent.record(
            "peer.rejected",
            json!({
                "peer": "peer-1",
                "cause": "unknown_device",
            }),
        );

        let content = std::fs::read_to_string(home.path().join(AUDIT_LOG_FILE)).unwrap();
        let line: Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(line["event"], "peer.rejected");
        assert_eq!(line["peer"], "peer-1");
        assert_eq!(line["cause"], "unknown_device");
        assert!(line["ts_ms"].as_u64().is_some());
        assert!(line.get("ticket").is_none());
        assert!(line.get("capability_secret").is_none());
    }

    #[test]
    fn local_audit_persists_security_relevant_hooks() {
        let home = tempdir().unwrap();
        let persistent = PersistentAudit::open(home.path()).unwrap();
        let sink = LocalAudit::new(persistent);
        let id = IdentityKey::from_bytes([0x11; 32]);

        sink.accepted(device(1), &id);
        sink.rejected(device(1), RejectCause::UnknownDevice);
        sink.blob_serve_rejected(device(1), BlobDenyCause::NotReferenced, Some([0x44; 32]));
        sink.active_member_threshold_reached(
            &iroh_rooms_core::event::ids::RoomId::from_bytes([0x55; 32]),
            4,
            5,
            1,
        );

        let content = std::fs::read_to_string(home.path().join(AUDIT_LOG_FILE)).unwrap();
        let events = content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["event"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                Value::String("peer.accepted".to_owned()),
                Value::String("peer.rejected".to_owned()),
                Value::String("blob.serve.rejected".to_owned()),
                Value::String("room.active_members.near_cap".to_owned()),
            ]
        );
        assert!(content.contains("\"hash_prefix\":\"44444444\""));
        assert!(content.contains("\"active\":4"));
        assert!(content.contains("\"max\":5"));
        assert!(content.contains("\"remaining\":1"));
        assert!(!content.contains("identity.secret"));
    }

    // The #122 line contract: a capability-proof accept or reject on the join
    // bootstrap must land in audit.ndjson (the shipped binary installs no
    // tracing subscriber, so this sink is the only durable record), carrying
    // the public peer id and never any part of the proof itself.
    #[test]
    fn local_audit_persists_capability_proof_outcomes() {
        let home = tempdir().unwrap();
        let persistent = PersistentAudit::open(home.path()).unwrap();
        let sink = LocalAudit::new(persistent);

        sink.bootstrap_capability_proven(device(1));
        sink.bootstrap_capability_rejected(device(2));

        let content = std::fs::read_to_string(home.path().join(AUDIT_LOG_FILE)).unwrap();
        let lines = content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "join.bootstrap.capability_proven");
        assert_eq!(lines[0]["peer"], device(1).to_string());
        assert_eq!(lines[1]["event"], "join.bootstrap.capability_rejected");
        assert_eq!(lines[1]["peer"], device(2).to_string());
        for line in &lines {
            assert!(line["ts_ms"].as_u64().is_some());
            // The proof is (invite_id, capability_secret); neither may ever be
            // written, in any spelling.
            assert!(line.get("invite_id").is_none());
            assert!(line.get("capability_secret").is_none());
            assert!(line.get("secret").is_none());
        }
    }
}
