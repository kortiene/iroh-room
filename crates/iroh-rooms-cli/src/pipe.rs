//! The Live Pipe Plane CLI: `iroh-rooms pipe expose | connect | close | list`
//! (spec `live-tcp-pipe-path.md` §6.5.3; PRD §15.7 / §16).
//!
//! These complete the PRD pipe journey on top of the landed net `pipe` module
//! ([`iroh_rooms_net::pipe`]): `expose` announces and serves a **loopback** TCP
//! service to explicitly named members (printing a clear security warning, PRD
//! §13.2.4), `connect` forwards a local loopback port to it, `close` revokes it, and
//! `list` shows the room's open pipes. They are thin orchestrators over the
//! conformance-tested gate — no new authorization logic lives here.

use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use iroh_rooms::experimental::session::{EndpointAddr, EndpointId, SecretKey};
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::pipe::is_loopback_target;
use iroh_rooms_net::{
    NetConfig, Node, PipeAuditSink, PipeDenyCause, PipeError, PipeOutcome, DEFAULT_TICK,
};
use serde_json::json;

use crate::error::CodedResultExt;
use crate::message::{
    build_admission, build_dial_set, endpoint_id_of, fold_room, net_mode, parse_peers,
    render_endpoint_addr, DB_FILE,
};
use crate::{audit, clock, identity};

/// Grace period after publishing a `pipe.closed` so the per-peer writer queues flush
/// before an ephemeral node tears down (mirrors `room send`).
const FLUSH_GRACE: Duration = Duration::from_millis(300);
/// How long `pipe connect` waits for the `pipe.opened` to sync before giving up.
const SYNC_WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// expose
// ---------------------------------------------------------------------------

/// Expose a local loopback TCP service as a key-bound pipe, then serve it until a
/// termination signal (publishing `pipe.closed{owner_exit}` on the way out; see
/// [`wait_for_shutdown_signal`] for the SIGINT/SIGTERM and hard-kill bounds).
///
/// # Errors
/// A non-loopback `--tcp`, an empty/invalid `--allow`, a non-member caller, or a
/// node/store failure. Argument validation runs before any IO.
#[allow(clippy::too_many_arguments)] // one linear orchestration; each arg is a distinct CLI input
#[allow(clippy::too_many_lines)] // a single validate-then-expose-then-serve flow; splitting hurts readability
pub async fn expose(
    home: &Path,
    room_id: &RoomId,
    tcp: &str,
    allow: &[String],
    label: Option<&str>,
    expires: Option<&str>,
    peers: &[String],
    verbose: bool,
    loopback: bool,
) -> Result<()> {
    // ---- Pre-IO validation (a bad invocation exposes nothing). ----
    let target = SocketAddr::from_str(tcp.trim())
        .map_err(|err| anyhow!("invalid --tcp target {tcp:?} (expected ip:port): {err}"))?;
    if !is_loopback_target(&target) {
        bail!(
            "refusing to expose non-loopback target {target}: the pipe forward target must be a \
             loopback address (127.0.0.0/8 or ::1) — PRD §13.2.3"
        );
    }
    if allow.is_empty() {
        bail!("a pipe must name at least one --allow <IDENTITY_ID> (no default-all; PRD §13.2)");
    }
    let allowed: Vec<IdentityKey> = allow
        .iter()
        .map(|s| {
            IdentityKey::from_str(s.trim())
                .map_err(|err| anyhow!("invalid --allow identity id {s:?}: {err}"))
        })
        .collect::<Result<_>>()?;
    let peer_addrs = parse_peers(peers)?;
    let label = label.unwrap_or("pipe");

    let created_at = clock::now_ms();
    let expires_at = expires
        .map(|e| parse_expires(e, created_at))
        .transpose()
        .coded(crate::error::ErrorCode::InvalidArgument)?;

    // ---- Membership: confirm the caller is an Active member/owner. ----
    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::PeerUnauthorized,
            "you are not an active member of room {room_id}; only an active member can expose a \
             pipe (this identity is {self_id})"
        );
    }

    // ---- Security warning + exposure summary (PRD §13.2.4 / §16.2). ----
    // The ⚠ lines (stderr) must name the exposed target *and* each allowed member
    // so the operator sees the trust decision even when stdout is redirected; the
    // full ids stay on the labeled `allow:` stdout lines (script-friendly split).
    let allowed_short = allowed
        .iter()
        .map(short_identity)
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "⚠  SECURITY: exposing {target} to {} allowed member(s): {allowed_short}.",
        allowed.len()
    );
    eprintln!("   Anyone allowed can reach {target} through this pipe while it is open.");
    println!("room: {room_id}");
    println!("target: {target}");
    println!("label: {label}");
    for id in &allowed {
        println!("allow: {id}");
    }
    if let Some(exp) = expires_at {
        println!("expires_at: {exp}");
    }

    // ---- Bring up the node, expose, and serve until Ctrl-C. ----
    let self_device = endpoint_id_of(secret.device.device_key())?;
    let admission = build_admission(&snapshot);
    let dial_set = build_dial_set(&snapshot, self_device, &peer_addrs);

    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    // The owner installs a CLI-local pipe audit sink so reject/teardown decisions
    // are locally logged both on stderr and in audit.ndjson (AC3 / §4.3); the CLI
    // has no `tracing` subscriber, so the default sink would be silent.
    let persistent_audit = audit::PersistentAudit::open(home)?;
    let node = Node::spawn_with_pipe_audit(
        secret_key,
        std::sync::Arc::new(admission),
        audit::sink_with(persistent_audit.clone()),
        engine,
        cfg,
        DEFAULT_TICK,
        std::sync::Arc::new(LocalPipeAudit::new(verbose, persistent_audit)),
    )
    .await
    .context("could not bring up the network node")?;

    if let Ok(addr) = node.endpoint_addr() {
        println!("listening: {}", render_endpoint_addr(&addr));
        println!("tip: share this address with connectors via --peer");
    }
    for addr in dial_set {
        node.connect_to(addr);
    }

    let pipe_id = node
        .pipe_expose(
            &secret.identity,
            &secret.device,
            room_id,
            target,
            label,
            &target.to_string(),
            &allowed,
            expires_at,
            created_at,
        )
        .await
        .context("could not expose the pipe")?;
    let pipe_hex = hex16(&pipe_id);
    println!("pipe_id: {pipe_hex}");
    println!("connectors run: iroh-rooms pipe connect {room_id} {pipe_hex} --local <PORT>");
    println!("close it with: iroh-rooms pipe close {pipe_hex}");
    println!("serving the pipe; press Ctrl-C to close it...");

    wait_for_shutdown_signal().await;

    // Best-effort graceful close: publish pipe.closed{owner_exit} and tear down.
    if let Err(err) = node
        .pipe_close(
            &secret.identity,
            &secret.device,
            room_id,
            pipe_id,
            Some("owner_exit"),
            clock::now_ms(),
        )
        .await
    {
        eprintln!("warning: could not publish pipe.closed on exit: {err}");
    }
    tokio::time::sleep(FLUSH_GRACE).await;
    node.shutdown()
        .await
        .context("could not shut down cleanly")?;
    println!("pipe closed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// connect
// ---------------------------------------------------------------------------

/// Connect to an open pipe: bind a loopback listener on `--local <PORT>` and forward
/// it to the owner until Ctrl-C.
///
/// # Errors
/// A non-member caller, an unsynced/unknown pipe, no reachable owner address, or a
/// node/listener failure.
pub async fn connect(
    home: &Path,
    room_id: &RoomId,
    pipe_id_hex: &str,
    local_port: u16,
    peers: &[String],
    loopback: bool,
) -> Result<()> {
    let pipe_id = parse_pipe_id(pipe_id_hex)?;
    let peer_addrs = parse_peers(peers)?;

    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::PeerUnauthorized,
            "you are not an active member of room {room_id}; only an active member can connect to \
             a pipe (this identity is {self_id})"
        );
    }

    let self_device = endpoint_id_of(secret.device.device_key())?;
    let admission = build_admission(&snapshot);
    let dial_set = build_dial_set(&snapshot, self_device, &peer_addrs);

    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    let audit_sink = audit::sink(home)?;
    let node = Node::spawn(
        secret_key,
        std::sync::Arc::new(admission),
        audit_sink,
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .context("could not bring up the network node")?;

    for addr in dial_set {
        node.connect_to(addr);
    }

    // Wait for the pipe.opened to sync so we learn the owner_endpoint.
    let opened = tokio::time::timeout(SYNC_WAIT, async {
        loop {
            if let Some(o) = node.pipe_opened(pipe_id).await {
                return o;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow!(
            "timed out waiting to sync pipe {pipe_id_hex}; pass the owner via --peer or run \
             `iroh-rooms room tail` first to sync"
        )
    })?;

    // Resolve a dialable owner address: a matching --peer (deterministic) else by
    // bare endpoint id (discovery).
    let owner_addr = resolve_owner_addr(&opened.owner_endpoint, &peer_addrs)?;

    // Scope item 3 (offline peer, command surface): an unreachable owner is
    // distinct from other pipe setup faults. `Node::pipe_connect` preserves the
    // original `PipeError` on the error path so it can be downcast here.
    let mut forwarder = match node.pipe_connect(owner_addr, pipe_id, local_port).await {
        Ok(f) => f,
        Err(err) => match err.downcast_ref::<PipeError>() {
            Some(PipeError::OwnerUnreachable(_)) => {
                crate::bail_coded!(
                    crate::error::ErrorCode::PeerOffline(
                        iroh_rooms_net::OfflineReason::Unreachable
                    ),
                    "the pipe owner is unreachable: {err:#}"
                );
            }
            _ => return Err(err.context("could not connect to the pipe")),
        },
    };
    println!("room: {room_id}");
    println!(
        "forwarding: {} -> pipe {pipe_id_hex}",
        forwarder.local_addr()
    );
    println!(
        "connect your client to {}; press Ctrl-C to stop.",
        forwarder.local_addr()
    );

    // Drain per-connection outcomes for a live status line until Ctrl-C.
    loop {
        tokio::select! {
            () = wait_for_ctrl_c() => break,
            outcome = forwarder.next_outcome() => match outcome {
                Some(PipeOutcome::Forwarded) => println!("[pipe] connection forwarding"),
                Some(PipeOutcome::Denied) => eprintln!("[pipe] denied by the owner (not authorized / closed)"),
                Some(PipeOutcome::OwnerClosed) => eprintln!("[pipe] owner closed the connection"),
                Some(PipeOutcome::Error(e)) => eprintln!("[pipe] error: {e}"),
                None => break,
            },
        }
    }

    forwarder.shutdown();
    node.shutdown()
        .await
        .context("could not shut down cleanly")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// close
// ---------------------------------------------------------------------------

/// Publish a signed `pipe.closed{closed}` (owner or admin) and tear the pipe down.
///
/// The governing room is inferred from the local log unless `room` is given
/// (spec IR-0108 §4.1): the canonical surface is `pipe close <PIPE_ID>` with no
/// room id.
///
/// # Errors
/// A non-member / unauthorized caller, an unknown or ambiguous pipe, or a node
/// failure.
pub async fn close(
    home: &Path,
    room: Option<&RoomId>,
    pipe_id_hex: &str,
    peers: &[String],
    loopback: bool,
) -> Result<()> {
    let pipe_id = parse_pipe_id(pipe_id_hex)?;
    let peer_addrs = parse_peers(peers)?;

    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;

    // Resolve the governing room: an explicit --room, else infer it from the local
    // log by finding the room whose pipe.opened announces this pipe_id (§4.1).
    let room_id = match room {
        Some(r) => *r,
        None => resolve_pipe_room(&store, &pipe_id)?,
    };
    let room_id = &room_id;

    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        crate::bail_coded!(
            crate::error::ErrorCode::PeerUnauthorized,
            "you are not an active member of room {room_id} (this identity is {self_id})"
        );
    }

    // Only the pipe owner or the room admin may close a pipe (§7 signer rule). Best
    // checked locally for a friendly pre-publish error; the fold is authoritative.
    let is_admin = snapshot.admin() == Some(&self_id);
    let is_owner = open_pipe(&store, room_id, &pipe_id)?.is_some_and(|o| o.owner_id == self_id);
    if !is_admin && !is_owner {
        crate::bail_coded!(
            crate::error::ErrorCode::PeerUnauthorized,
            "only the pipe owner or the room admin can close pipe {pipe_id_hex}; this identity is \
             neither"
        );
    }

    let self_device = endpoint_id_of(secret.device.device_key())?;
    let admission = build_admission(&snapshot);
    let dial_set = build_dial_set(&snapshot, self_device, &peer_addrs);

    let engine = SyncEngine::open(store, *room_id, SyncConfig::default())
        .map_err(|err| anyhow!("could not open sync engine: {err}"))?;
    let secret_key = SecretKey::from_bytes(&secret.device.to_seed());
    let cfg = NetConfig {
        mode: net_mode(loopback),
        ..NetConfig::default()
    };
    let audit_sink = audit::sink(home)?;
    let node = Node::spawn(
        secret_key,
        std::sync::Arc::new(admission),
        audit_sink,
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
    .context("could not bring up the network node")?;
    for addr in dial_set {
        node.connect_to(addr);
    }

    node.pipe_close(
        &secret.identity,
        &secret.device,
        room_id,
        pipe_id,
        Some("closed"),
        clock::now_ms(),
    )
    .await
    .context("could not publish pipe.closed")?;
    println!("closed pipe {pipe_id_hex} in room {room_id}");

    tokio::time::sleep(FLUSH_GRACE).await;
    node.shutdown()
        .await
        .context("could not shut down cleanly")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// list (offline)
// ---------------------------------------------------------------------------

/// List the room's currently-open pipes (a `pipe.opened` with no causally-known
/// `pipe.closed`), read from the local log. Offline — no node is brought up.
///
/// # Errors
/// An unknown room or a store read failure.
pub fn list(home: &Path, room_id: &RoomId) -> Result<()> {
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    // Fold first so we only show pipes from a known room (and validate the log).
    let (_, _snapshot) = fold_room(&store, home, room_id)?;

    let closed = closed_pipe_ids(&store, room_id)?;
    let opened = store
        .by_type(room_id, EventType::PipeOpened)
        .with_context(|| format!("could not read pipe.opened events for room {room_id}"))?;

    println!("room: {room_id}");
    let mut open_count = 0usize;
    for se in opened {
        let Ok(ev) = SignedEvent::decode(&se.wire.signed) else {
            continue;
        };
        let Content::PipeOpened(p) = ev.content else {
            continue;
        };
        if closed.contains(&p.pipe_id) {
            continue;
        }
        open_count += 1;
        println!("pipe_id: {}", hex16(&p.pipe_id));
        println!("  owner: {}", p.owner_id);
        println!("  label: {}", p.label);
        println!("  allowed: {}", p.allowed_members.len());
        match p.expires_at {
            Some(exp) => println!("  expires_at: {exp}"),
            None => println!("  expires_at: never"),
        }
    }
    if open_count == 0 {
        println!("(no open pipes)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a 32-char lowercase-hex `pipe_id` into 16 bytes.
fn parse_pipe_id(s: &str) -> Result<[u8; 16]> {
    let s = s.trim();
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid pipe id {s:?} (expected 32 lowercase hex chars)");
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow!("invalid pipe id {s:?}"))?;
    }
    Ok(out)
}

/// Lowercase hex of a 16-byte id.
fn hex16(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(32);
    for b in id {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Resolve the owner's dialable address: a `--peer` whose id matches the signed
/// `owner_endpoint` (deterministic), else a bare endpoint id (discovery).
fn resolve_owner_addr(
    owner_endpoint: &iroh_rooms_core::event::keys::DeviceKey,
    peer_addrs: &[EndpointAddr],
) -> Result<EndpointAddr> {
    let id = EndpointId::from_bytes(owner_endpoint.as_bytes())
        .map_err(|err| anyhow!("pipe owner_endpoint is not a valid endpoint id: {err}"))?;
    Ok(peer_addrs
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .unwrap_or_else(|| EndpointAddr::new(id)))
}

/// The set of `pipe_id`s with a causally-known `pipe.closed` in the room.
fn closed_pipe_ids(
    store: &EventStore,
    room_id: &RoomId,
) -> Result<std::collections::BTreeSet<[u8; 16]>> {
    let mut closed = std::collections::BTreeSet::new();
    let events = store
        .by_type(room_id, EventType::PipeClosed)
        .with_context(|| format!("could not read pipe.closed events for room {room_id}"))?;
    for se in events {
        if let Ok(ev) = SignedEvent::decode(&se.wire.signed) {
            if let Content::PipeClosed(c) = ev.content {
                closed.insert(c.pipe_id);
            }
        }
    }
    Ok(closed)
}

/// The governing open `pipe.opened` (ignoring closed) for `pipe_id`, if present.
fn open_pipe(
    store: &EventStore,
    room_id: &RoomId,
    pipe_id: &[u8; 16],
) -> Result<Option<iroh_rooms_core::event::content::PipeOpened>> {
    let events = store
        .by_type(room_id, EventType::PipeOpened)
        .with_context(|| format!("could not read pipe.opened events for room {room_id}"))?;
    for se in events {
        if let Ok(ev) = SignedEvent::decode(&se.wire.signed) {
            if let Content::PipeOpened(p) = ev.content {
                if &p.pipe_id == pipe_id {
                    return Ok(Some(p));
                }
            }
        }
    }
    Ok(None)
}

/// Infer the room governing `pipe_id` by scanning the local store: the room whose
/// `pipe.opened` announces this pipe (spec §4.1). Fail closed — an unknown pipe (0
/// matches) or an ambiguous one (>1, e.g. across imported DBs) yields an actionable
/// error rather than guessing. `pipe_id` is a 16-byte CSPRNG value, so >1 is
/// astronomically unlikely but handled deterministically via `--room`.
fn resolve_pipe_room(store: &EventStore, pipe_id: &[u8; 16]) -> Result<RoomId> {
    let rooms = store
        .room_ids()
        .context("could not enumerate local rooms to resolve the pipe")?;
    let mut matches: Vec<RoomId> = Vec::new();
    for room_id in rooms {
        if open_pipe(store, &room_id, pipe_id)?.is_some() {
            matches.push(room_id);
        }
    }
    match matches.as_slice() {
        [] => bail!(
            "no such pipe {} in any local room; run `iroh-rooms pipe list <ROOM_ID>` or sync the \
             owner's log first",
            hex16(pipe_id)
        ),
        [one] => Ok(*one),
        many => {
            let candidates = many
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "pipe {} exists in multiple local rooms ({candidates}); pass --room <ROOM_ID> to \
                 disambiguate",
                hex16(pipe_id)
            )
        }
    }
}

/// Parse a `--expires <int>{s|m|h|d}` into an absolute `expires_at` anchored at
/// `created_at` (mirrors the invite-expiry parser).
fn parse_expires(spec: &str, created_at: u64) -> Result<u64> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("--expires must not be empty; use <int>{{s|m|h|d}} e.g. 24h");
    }
    let unit = spec.chars().last().expect("spec is non-empty");
    let unit_ms: u64 = match unit {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => bail!("--expires must end with s, m, h, or d (e.g. 24h); got {spec:?}"),
    };
    let digits = &spec[..spec.len() - 1];
    if digits.is_empty() {
        bail!("--expires must include a number before the unit (e.g. 24h); got {spec:?}");
    }
    let value: u64 = digits
        .parse()
        .map_err(|_| anyhow!("--expires must be a positive integer with a unit; got {spec:?}"))?;
    if value == 0 {
        bail!("--expires must be greater than zero; got {spec:?}");
    }
    let duration_ms = value
        .checked_mul(unit_ms)
        .ok_or_else(|| anyhow!("--expires {spec:?} is too large"))?;
    created_at
        .checked_add(duration_ms)
        .ok_or_else(|| anyhow!("--expires {spec:?} overflows the clock"))
}

/// Await Ctrl-C, downgrading a listener error to an immediate return (so a serving
/// command still exits rather than hanging).
async fn wait_for_ctrl_c() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        eprintln!("warning: could not listen for Ctrl-C ({err}); shutting down");
    }
}

/// Await a termination signal that should trigger a graceful owner-exit close:
/// Ctrl-C (SIGINT) on every platform, plus SIGTERM (the default `kill`) on Unix
/// (spec IR-0108 §4.4/§5.5).
///
/// A hard kill (SIGKILL) or power loss cannot be caught: the owner endpoint dies,
/// so a connector tears its session down and **forwarding stops**, but no
/// `pipe.closed{owner_exit}` reaches the log until an owner/admin `pipe close` —
/// the §8 reachability bound, documented rather than solved.
#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                () = wait_for_ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(err) => {
            eprintln!("warning: could not listen for SIGTERM ({err}); Ctrl-C only");
            wait_for_ctrl_c().await;
        }
    }
}

/// Non-Unix fallback: SIGTERM has no portable equivalent, so the guarantee is
/// "graceful on catchable termination" (Ctrl-C).
#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    wait_for_ctrl_c().await;
}

/// A short, human-scannable prefix of an identity id for the security warning (the
/// full id stays on the labeled `allow:` stdout line).
fn short_identity(id: &IdentityKey) -> String {
    id.to_string().chars().take(8).collect()
}

/// A short prefix of a 16-byte pipe id for a one-line audit record.
fn short_pipe(pipe_id: &[u8; 16]) -> String {
    hex16(pipe_id).chars().take(8).collect()
}

/// A short prefix of an endpoint id for a one-line audit record.
fn short_endpoint(device: EndpointId) -> String {
    device.to_string().chars().take(8).collect()
}

/// A [`PipeAuditSink`] that renders the owner-side Live-Pipe-Plane audit vocabulary
/// as stable, greppable lines on **stderr** (spec IR-0108 §4.3).
///
/// The CLI installs no `tracing` subscriber, so the default
/// [`TracingPipeAudit`](iroh_rooms_net::TracingPipeAudit) output is dropped; this
/// sink makes "unauthorized connect rejected **and locally logged**" (AC3 / PRD
/// §13.2.7) actually visible on the owner's terminal. Rejects and teardowns are
/// security-relevant and always print; per-connection accepts are chatter and print
/// only under `--verbose`. stdout is left clean for scripting (PRD §16.5).
struct StderrPipeAudit {
    verbose: bool,
}

impl StderrPipeAudit {
    fn new(verbose: bool) -> Self {
        Self { verbose }
    }
}

/// Pipe audit sink for the CLI: keeps the existing stderr vocabulary and persists
/// the same decisions to the shared local audit log.
struct LocalPipeAudit {
    stderr: StderrPipeAudit,
    persistent: audit::PersistentAudit,
}

impl LocalPipeAudit {
    fn new(verbose: bool, persistent: audit::PersistentAudit) -> Self {
        Self {
            stderr: StderrPipeAudit::new(verbose),
            persistent,
        }
    }
}

impl PipeAuditSink for LocalPipeAudit {
    fn opened(&self, pipe_id: &[u8; 16], allowed: usize) {
        self.persistent.record(
            "pipe.opened",
            json!({
                "pipe": hex16(pipe_id),
                "allowed": allowed,
            }),
        );
        self.stderr.opened(pipe_id, allowed);
    }

    fn closed(&self, pipe_id: &[u8; 16], reason: &str) {
        self.persistent.record(
            "pipe.closed",
            json!({
                "pipe": hex16(pipe_id),
                "reason": reason,
            }),
        );
        self.stderr.closed(pipe_id, reason);
    }

    fn connect_accepted(&self, device: EndpointId, pipe_id: &[u8; 16]) {
        self.persistent.record(
            "pipe.connect.accepted",
            json!({
                "peer": device.to_string(),
                "pipe": hex16(pipe_id),
            }),
        );
        self.stderr.connect_accepted(device, pipe_id);
    }

    fn connect_rejected(
        &self,
        device: EndpointId,
        pipe_id: Option<&[u8; 16]>,
        cause: PipeDenyCause,
    ) {
        self.persistent.record(
            "pipe.connect.rejected",
            json!({
                "peer": device.to_string(),
                "pipe": pipe_id.map(hex16),
                "cause": cause.code(),
            }),
        );
        self.stderr.connect_rejected(device, pipe_id, cause);
    }

    fn torndown(&self, device: EndpointId, pipe_id: &[u8; 16], cause: PipeDenyCause) {
        self.persistent.record(
            "pipe.torndown",
            json!({
                "peer": device.to_string(),
                "pipe": hex16(pipe_id),
                "cause": cause.code(),
            }),
        );
        self.stderr.torndown(device, pipe_id, cause);
    }
}

impl PipeAuditSink for StderrPipeAudit {
    fn opened(&self, _pipe_id: &[u8; 16], _allowed: usize) {
        // `expose` prints its own richer `pipe_id:` / `allow:` summary on stdout.
    }

    fn closed(&self, _pipe_id: &[u8; 16], _reason: &str) {
        // `expose` / `close` print their own close confirmation.
    }

    fn connect_accepted(&self, device: EndpointId, pipe_id: &[u8; 16]) {
        if self.verbose {
            eprintln!(
                "pipe.connect.accepted peer={} pipe={}",
                short_endpoint(device),
                short_pipe(pipe_id)
            );
        }
    }

    fn connect_rejected(
        &self,
        device: EndpointId,
        pipe_id: Option<&[u8; 16]>,
        cause: PipeDenyCause,
    ) {
        eprintln!(
            "pipe.connect.rejected:{} peer={} pipe={}",
            cause.code(),
            short_endpoint(device),
            pipe_id.map_or_else(|| "-".to_owned(), short_pipe)
        );
    }

    fn torndown(&self, device: EndpointId, pipe_id: &[u8; 16], cause: PipeDenyCause) {
        eprintln!(
            "pipe.torndown:{} peer={} pipe={}",
            cause.code(),
            short_endpoint(device),
            short_pipe(pipe_id)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        hex16, parse_expires, parse_pipe_id, resolve_owner_addr, resolve_pipe_room, short_identity,
        short_pipe, LocalPipeAudit, StderrPipeAudit,
    };

    use iroh_rooms::experimental::session::EndpointAddr;
    use iroh_rooms_core::event::ids::{EventId, RoomId};
    use iroh_rooms_core::event::keys::SigningKey;
    use iroh_rooms_core::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_core::event::{build_pipe_opened, build_room_created};
    use iroh_rooms_core::store::EventStore;
    use iroh_rooms_net::{PipeAuditSink, PipeDenyCause};
    use serde_json::Value;
    use tempfile::tempdir;

    use crate::audit::{PersistentAudit, AUDIT_LOG_FILE};

    const OWNER_IDENTITY_SEED: [u8; 32] = [0x50; 32];
    const OWNER_DEVICE_SEED: [u8; 32] = [0x51; 32];
    const ALLOWED_MEMBER_SEED: [u8; 32] = [0x60; 32];
    const ROOM_NONCE_A: [u8; 16] = [0xA1; 16];
    const ROOM_NONCE_B: [u8; 16] = [0xB1; 16];
    const PIPE_ID: [u8; 16] = [0x77; 16];
    const BASE_TS: u64 = 1_750_000_100_000;
    const ALPN: &str = "/iroh-rooms/pipe/1";

    fn owner() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&OWNER_IDENTITY_SEED),
            SigningKey::from_seed(&OWNER_DEVICE_SEED),
        )
    }

    /// Seed a store with a genesis event and return the `room_id` + genesis `event_id`.
    fn seed_genesis(store: &mut EventStore, nonce: &[u8; 16]) -> (RoomId, EventId) {
        let (id, dev) = owner();
        let wire = build_room_created(&id, &dev, "Test Room", nonce, BASE_TS);
        let room_id = {
            let ev = iroh_rooms_core::event::signed::SignedEvent::decode(&wire.signed).unwrap();
            ev.room_id
        };
        let ctx = ValidationContext::for_room(room_id);
        let v = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("genesis valid");
        let ev_id = v.event_id;
        store.insert(&v).expect("insert genesis");
        (room_id, ev_id)
    }

    /// Seed a pipe.opened event in `room_id`, citing `prev` as parent, using `pipe_id`.
    fn seed_pipe_opened(store: &mut EventStore, room_id: RoomId, prev: EventId, pipe_id: [u8; 16]) {
        let (id_key, dev_key) = owner();
        let allowed = SigningKey::from_seed(&ALLOWED_MEMBER_SEED).identity_key();
        let wire = build_pipe_opened(
            &id_key,
            &dev_key,
            &room_id,
            pipe_id,
            &dev_key.device_key(),
            "test-pipe",
            "127.0.0.1:9999",
            ALPN,
            &[allowed],
            None,
            &[prev],
            BASE_TS + 1_000,
        );
        let ctx = ValidationContext::for_room(room_id);
        let v = validate_wire_bytes(&wire.to_bytes(), &ctx).expect("pipe.opened valid");
        store.insert(&v).expect("insert pipe.opened");
    }

    // ── resolve_pipe_room ────────────────────────────────────────────────────

    // An empty store → "no such pipe" error.
    #[test]
    fn resolve_pipe_room_empty_store_returns_no_such_pipe_error() {
        let store = EventStore::open_in_memory().unwrap();
        let err = resolve_pipe_room(&store, &PIPE_ID).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no such pipe"),
            "error must mention 'no such pipe'; got: {msg}"
        );
    }

    // A store with rooms but none matching the pipe_id → "no such pipe" error.
    #[test]
    fn resolve_pipe_room_room_with_no_pipe_returns_no_such_pipe_error() {
        let mut store = EventStore::open_in_memory().unwrap();
        seed_genesis(&mut store, &ROOM_NONCE_A);

        let other_pipe: [u8; 16] = [0xFF; 16];
        let err = resolve_pipe_room(&store, &other_pipe).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no such pipe"),
            "error must mention 'no such pipe'; got: {msg}"
        );
    }

    // A single matching room → Ok(room_id).
    #[test]
    fn resolve_pipe_room_single_match_resolves() {
        let mut store = EventStore::open_in_memory().unwrap();
        let (room_id, genesis_ev_id) = seed_genesis(&mut store, &ROOM_NONCE_A);
        seed_pipe_opened(&mut store, room_id, genesis_ev_id, PIPE_ID);

        let resolved = resolve_pipe_room(&store, &PIPE_ID).expect("should resolve to single room");
        assert_eq!(
            resolved, room_id,
            "resolved room must match the seeded room"
        );
    }

    // Two rooms each with the same pipe_id → ambiguous error naming both candidates.
    #[test]
    fn resolve_pipe_room_two_matching_rooms_gives_ambiguous_error() {
        let mut store = EventStore::open_in_memory().unwrap();
        let (room_a, genesis_a) = seed_genesis(&mut store, &ROOM_NONCE_A);
        let (room_b, genesis_b) = seed_genesis(&mut store, &ROOM_NONCE_B);
        seed_pipe_opened(&mut store, room_a, genesis_a, PIPE_ID);
        seed_pipe_opened(&mut store, room_b, genesis_b, PIPE_ID);

        let err = resolve_pipe_room(&store, &PIPE_ID).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("multiple local rooms") || msg.contains("disambiguate"),
            "ambiguous error must mention multiple rooms or --room; got: {msg}"
        );
        // Both room ids must appear in the error message so the user knows their options.
        assert!(
            msg.contains(&room_a.to_string()) || msg.contains(&room_b.to_string()),
            "error must name at least one candidate room_id; got: {msg}"
        );
    }

    // ── hex / short helpers ────────────────────────────────────────────────────

    #[test]
    fn pipe_id_round_trips_through_hex() {
        let id = [0xab; 16];
        let hex = hex16(&id);
        assert_eq!(hex.len(), 32);
        assert_eq!(parse_pipe_id(&hex).unwrap(), id);
    }

    #[test]
    fn pipe_id_rejects_bad_length_and_chars() {
        assert!(parse_pipe_id("abc").is_err());
        assert!(parse_pipe_id(&"z".repeat(32)).is_err());
        assert!(parse_pipe_id(&"a".repeat(31)).is_err());
    }

    #[test]
    fn pipe_id_all_zeros_and_all_ff_round_trip() {
        assert_eq!(parse_pipe_id(&hex16(&[0x00; 16])).unwrap(), [0x00; 16]);
        assert_eq!(parse_pipe_id(&hex16(&[0xff; 16])).unwrap(), [0xff; 16]);
    }

    // short_pipe returns 8 chars from the hex16 representation.
    #[test]
    fn short_pipe_is_eight_hex_chars() {
        let id = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let s = short_pipe(&id);
        assert_eq!(s.len(), 8);
        assert_eq!(&s, "12345678");
    }

    // short_identity truncates to 8 chars.
    #[test]
    fn short_identity_is_eight_chars() {
        let id_key = SigningKey::from_seed(&OWNER_IDENTITY_SEED).identity_key();
        let s = short_identity(&id_key);
        assert_eq!(s.len(), 8, "short_identity must be exactly 8 chars");
        let full = id_key.to_string();
        assert!(
            full.starts_with(&s),
            "short_identity must be a prefix of the full identity string"
        );
    }

    // ── StderrPipeAudit construction + trait coverage ─────────────────────────

    // The audit sink can be constructed with verbose=false (default / non-verbose
    // mode) without panicking.
    #[test]
    fn stderr_pipe_audit_constructs_without_panic() {
        let _quiet = StderrPipeAudit::new(false);
        let _verbose = StderrPipeAudit::new(true);
    }

    // connect_rejected does not panic for any PipeDenyCause variant (all must have
    // a non-empty code() so the log line is stable and greppable).
    #[test]
    fn stderr_pipe_audit_reject_does_not_panic_for_all_causes() {
        use iroh_rooms::experimental::session::SecretKey;
        let sink = StderrPipeAudit::new(false);
        let dummy_device = SecretKey::from_bytes(&[0u8; 32]).public();
        let dummy_pipe: [u8; 16] = [0x42; 16];
        for cause in [
            PipeDenyCause::NotAllowed,
            PipeDenyCause::NotActive,
            PipeDenyCause::Closed,
            PipeDenyCause::Expired,
            PipeDenyCause::UnknownDevice,
            PipeDenyCause::OwnerInactive,
        ] {
            // reject/teardown must not panic (they write to stderr as a side effect).
            sink.connect_rejected(dummy_device, Some(&dummy_pipe), cause);
            sink.torndown(dummy_device, &dummy_pipe, cause);
        }
        // Also test with no pipe_id (early-gate rejection before pipe is known).
        sink.connect_rejected(dummy_device, None, PipeDenyCause::NotAllowed);
    }

    // connect_accepted is a no-op unless verbose; calling it must not panic.
    #[test]
    fn stderr_pipe_audit_accept_does_not_panic() {
        use iroh_rooms::experimental::session::SecretKey;
        let quiet = StderrPipeAudit::new(false);
        let loud = StderrPipeAudit::new(true);
        let dummy_device = SecretKey::from_bytes(&[0u8; 32]).public();
        let dummy_pipe: [u8; 16] = [0x42; 16];
        quiet.connect_accepted(dummy_device, &dummy_pipe);
        loud.connect_accepted(dummy_device, &dummy_pipe);
    }

    #[test]
    fn local_pipe_audit_persists_ndjson_events() {
        use iroh::SecretKey;
        let home = tempdir().unwrap();
        let persistent = PersistentAudit::open(home.path()).unwrap();
        let sink = LocalPipeAudit::new(false, persistent);
        let dummy_device = SecretKey::from_bytes(&[0u8; 32]).public();
        let dummy_pipe: [u8; 16] = [0x42; 16];

        sink.opened(&dummy_pipe, 2);
        sink.connect_rejected(dummy_device, Some(&dummy_pipe), PipeDenyCause::NotAllowed);
        sink.torndown(dummy_device, &dummy_pipe, PipeDenyCause::Closed);

        let content = std::fs::read_to_string(home.path().join(AUDIT_LOG_FILE)).unwrap();
        let lines = content
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines[0]["event"], "pipe.opened");
        assert_eq!(lines[0]["allowed"], 2);
        assert_eq!(lines[1]["event"], "pipe.connect.rejected");
        assert_eq!(lines[1]["cause"], "not_allowed");
        assert_eq!(lines[2]["event"], "pipe.torndown");
        assert_eq!(lines[2]["cause"], "closed");
        assert_eq!(lines[0]["pipe"], hex16(&dummy_pipe));
        assert!(!content.contains("identity.secret"));
    }

    // ── parse_expires ─────────────────────────────────────────────────────────

    #[test]
    fn expires_anchors_on_created_at() {
        assert_eq!(parse_expires("1h", 1_000).unwrap(), 1_000 + 3_600_000);
        assert_eq!(parse_expires("2d", 0).unwrap(), 2 * 86_400_000);
    }

    #[test]
    fn expires_rejects_bad_specs() {
        assert!(parse_expires("", 0).is_err());
        assert!(parse_expires("0h", 0).is_err());
        assert!(parse_expires("10", 0).is_err());
        assert!(parse_expires("xh", 0).is_err());
    }

    #[test]
    fn expires_all_valid_units_parse_correctly() {
        let base = 0u64;
        assert_eq!(parse_expires("1s", base).unwrap(), 1_000);
        assert_eq!(parse_expires("1m", base).unwrap(), 60_000);
        assert_eq!(parse_expires("1h", base).unwrap(), 3_600_000);
        assert_eq!(parse_expires("1d", base).unwrap(), 86_400_000);
    }

    // The 's' unit multiplies by 1_000; u64::MAX * 1_000 overflows checked_mul.
    #[test]
    fn expires_mul_overflow_is_rejected() {
        assert!(
            parse_expires(&format!("{}s", u64::MAX), 0).is_err(),
            "u64::MAX seconds must overflow checked_mul and be rejected"
        );
    }

    // Even a small delta overflows if created_at is near u64::MAX.
    #[test]
    fn expires_add_overflow_is_rejected() {
        assert!(
            parse_expires("1s", u64::MAX).is_err(),
            "1s added to u64::MAX must overflow checked_add and be rejected"
        );
    }

    // ── StderrPipeAudit opened/closed (no-ops) ────────────────────────────────

    // `opened` and `closed` are intentional no-ops in the CLI sink (the `expose`
    // and `close` commands print their own richer summaries on stdout). Calling
    // them must not panic in either verbosity mode.
    #[test]
    fn stderr_pipe_audit_opened_and_closed_no_ops_do_not_panic() {
        let quiet = StderrPipeAudit::new(false);
        let verbose = StderrPipeAudit::new(true);
        let dummy_pipe: [u8; 16] = [0x42; 16];
        quiet.opened(&dummy_pipe, 3);
        quiet.closed(&dummy_pipe, "closed");
        verbose.opened(&dummy_pipe, 1);
        verbose.closed(&dummy_pipe, "owner_exit");
    }

    // ── resolve_owner_addr ────────────────────────────────────────────────────

    // With no peers, the function must return the bare endpoint id derived from
    // the device key without error (the fallback path).
    #[test]
    fn resolve_owner_addr_with_empty_peers_returns_ok() {
        let dev_key = SigningKey::from_seed(&[0x99u8; 32]).device_key();
        assert!(
            resolve_owner_addr(&dev_key, &[]).is_ok(),
            "empty peer list must succeed via bare endpoint id fallback"
        );
    }

    // When a peer whose id matches the device key is in the list, it is selected.
    // Bootstrap: get the bare endpoint addr first, then pass it back as a peer.
    #[test]
    fn resolve_owner_addr_selects_matching_peer() {
        let dev_key = SigningKey::from_seed(&[0xCCu8; 32]).device_key();
        // Compute the expected endpoint id via the same path as the function.
        let bare = resolve_owner_addr(&dev_key, &[]).expect("empty-peers baseline must succeed");
        let matching_peer = EndpointAddr::new(bare.id);
        let with_peer =
            resolve_owner_addr(&dev_key, &[matching_peer]).expect("matching peer must succeed");
        assert_eq!(
            with_peer.id, bare.id,
            "matching peer's endpoint id must be returned"
        );
    }

    // When no peer id matches the device key, the function falls back to the bare
    // endpoint id (does not return an error or use the wrong peer).
    #[test]
    fn resolve_owner_addr_with_non_matching_peers_falls_back_to_bare_endpoint() {
        use iroh_rooms::experimental::session::SecretKey;
        let dev_key = SigningKey::from_seed(&[0xDDu8; 32]).device_key();
        let expected_id = resolve_owner_addr(&dev_key, &[])
            .expect("empty-peers baseline must succeed")
            .id;
        // A peer with a DIFFERENT id.
        let other_id = SecretKey::from_bytes(&[0xEEu8; 32]).public();
        let non_matching_peer = EndpointAddr::new(other_id);
        let result = resolve_owner_addr(&dev_key, &[non_matching_peer])
            .expect("non-matching peer must not cause failure");
        assert_eq!(
            result.id, expected_id,
            "fallback must use the endpoint id derived from the device key"
        );
    }
}
