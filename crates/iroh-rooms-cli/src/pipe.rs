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
use iroh::{EndpointAddr, SecretKey};
use iroh_rooms_core::event::content::{Content, EventType};
use iroh_rooms_core::event::ids::RoomId;
use iroh_rooms_core::event::keys::IdentityKey;
use iroh_rooms_core::event::signed::SignedEvent;
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::pipe::is_loopback_target;
use iroh_rooms_net::{NetConfig, Node, PipeOutcome, TracingAudit, DEFAULT_TICK};

use crate::message::{
    build_admission, build_dial_set, endpoint_id_of, fold_room, net_mode, parse_peers,
    render_endpoint_addr, DB_FILE,
};
use crate::{clock, identity};

/// Grace period after publishing a `pipe.closed` so the per-peer writer queues flush
/// before an ephemeral node tears down (mirrors `room send`).
const FLUSH_GRACE: Duration = Duration::from_millis(300);
/// How long `pipe connect` waits for the `pipe.opened` to sync before giving up.
const SYNC_WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// expose
// ---------------------------------------------------------------------------

/// Expose a local loopback TCP service as a key-bound pipe, then serve it until
/// Ctrl-C (publishing `pipe.closed{owner_exit}` on the way out).
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
    let expires_at = expires.map(|e| parse_expires(e, created_at)).transpose()?;

    // ---- Membership: confirm the caller is an Active member/owner. ----
    let secret = identity::SecretKeys::load(home)?;
    let self_id = secret.identity.identity_key();
    let db_path = home.join(DB_FILE);
    let store = EventStore::open(&db_path)
        .with_context(|| format!("could not open event store at {}", db_path.display()))?;
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        bail!(
            "you are not an active member of room {room_id}; only an active member can expose a \
             pipe (this identity is {self_id})"
        );
    }

    // ---- Security warning + exposure summary (PRD §13.2.4 / §16.2). ----
    eprintln!("⚠  SECURITY: exposing a local service to named room members.");
    eprintln!("   Anyone you allow can reach {target} through this pipe while it is open.");
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
    let node = Node::spawn(
        secret_key,
        std::sync::Arc::new(admission),
        std::sync::Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
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
    println!("close it with: iroh-rooms pipe close {room_id} {pipe_hex}");
    println!("serving the pipe; press Ctrl-C to close it...");

    wait_for_ctrl_c().await;

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
        bail!(
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
    let node = Node::spawn(
        secret_key,
        std::sync::Arc::new(admission),
        std::sync::Arc::new(TracingAudit),
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

    let mut forwarder = node
        .pipe_connect(owner_addr, pipe_id, local_port)
        .await
        .context("could not connect to the pipe")?;
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
/// # Errors
/// A non-member / unauthorized caller, an unknown pipe, or a node failure.
pub async fn close(
    home: &Path,
    room_id: &RoomId,
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
    let (_, snapshot) = fold_room(&store, home, room_id)?;
    if !snapshot.is_active(&self_id) {
        bail!("you are not an active member of room {room_id} (this identity is {self_id})");
    }

    // Only the pipe owner or the room admin may close a pipe (§7 signer rule). Best
    // checked locally for a friendly pre-publish error; the fold is authoritative.
    let is_admin = snapshot.admin() == Some(&self_id);
    let is_owner = open_pipe(&store, room_id, &pipe_id)?.is_some_and(|o| o.owner_id == self_id);
    if !is_admin && !is_owner {
        bail!(
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
    let node = Node::spawn(
        secret_key,
        std::sync::Arc::new(admission),
        std::sync::Arc::new(TracingAudit),
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
    let id = iroh::EndpointId::from_bytes(owner_endpoint.as_bytes())
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

#[cfg(test)]
mod tests {
    use super::{hex16, parse_expires, parse_pipe_id};

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
}
