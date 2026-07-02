//! **The example agent** (issue #39 / IR-0304): a minimal, runnable Rust agent
//! that drives an Iroh Rooms room *through the SDK* — the intended integration
//! model — rather than by shelling out to the `iroh-rooms` binary. It sets up
//! its own local identity, joins a room by ticket, posts a signed
//! `agent.status` update, and (optionally) shares one artifact.
//!
//! This is the runnable evolution of `03_invite_and_join.rs` +
//! `07_agent_status.rs`: unlike those compile-only fragments, this example
//! takes real command-line arguments and a **persisted** identity, so it can
//! actually redeem a real invite ticket — which is bound to a specific, named
//! identity, so a freshly generated random key (the `07` seed's shortcut) can
//! never do that. See `README.md` in this directory for the full
//! clean-checkout run flow and an adaptation guide.
//!
//! Requires: `--features experimental`. Two subcommands:
//!
//! ```text
//! example_agent identity [--identity-file <PATH>] [--force]
//! example_agent join --ticket <ROOMTKT> [--identity-file <PATH>] [--peer <ENDPOINT_ADDR>]...
//!                     [--status <STATE>] [--message <TEXT>] [--progress <0..100>]
//!                     [--artifact <PATH>] [--loopback]
//! ```
//!
//! **Authorization posture (issue AC3).** The only capability this agent holds
//! is the room membership its invite ticket granted. It admits/dials no one
//! but the room admin until the fold teaches it otherwise, joins at the
//! ticket's role (expected `agent`, the least-privileged role in the
//! `Agent < Member < Admin` lattice), and authors only `member.joined`,
//! `agent.status`, and (optionally) `file.shared` — every one of them
//! re-gated by the same fold check every peer in the room runs. There are no
//! central-service credentials anywhere: just this locally generated keypair
//! and the ticket. Remove the agent (`member.removed`) or let its invite
//! expire, and it can do nothing.

#[cfg(feature = "experimental")]
use std::net::SocketAddr;
#[cfg(feature = "experimental")]
use std::path::{Path, PathBuf};
#[cfg(feature = "experimental")]
use std::sync::Arc;
#[cfg(feature = "experimental")]
use std::time::Duration;

#[cfg(feature = "experimental")]
use anyhow::Context;
#[cfg(feature = "experimental")]
use iroh::{EndpointAddr, EndpointId, SecretKey};
#[cfg(feature = "experimental")]
use iroh_rooms::events::{
    build_agent_status, constants::SHORT_ID_LEN, validate_wire_bytes, HashRef, ValidationContext,
};
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::blob::BlobStore;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::session::{
    AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, TracingAudit, DEFAULT_TICK,
};
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::store::EventStore;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
#[cfg(feature = "experimental")]
use iroh_rooms::files::build_file_shared;
#[cfg(feature = "experimental")]
use iroh_rooms::identity::{DeviceBinding, IdentityKey, SigningKey};
#[cfg(feature = "experimental")]
use iroh_rooms::room::{build_member_joined, RoomInviteTicket};

/// Default `--identity-file` (relative to the current directory).
#[cfg(feature = "experimental")]
const DEFAULT_IDENTITY_FILE: &str = "./example-agent.identity";
/// Default `--status` label when none is passed.
#[cfg(feature = "experimental")]
const DEFAULT_STATUS: &str = "running_tests";
/// The `member.joined.display_name` this agent identifies itself with.
#[cfg(feature = "experimental")]
const AGENT_DISPLAY_NAME: &str = "example-agent";
/// Time budget for connecting to the admin, bootstrapping membership, and
/// confirming the local `Active` transition (mirrors `iroh-rooms room join`'s
/// default timeout).
#[cfg(feature = "experimental")]
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting on a bounded membership-convergence deadline.
#[cfg(feature = "experimental")]
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Grace after the last publish so it flushes to a connected peer before this
/// short-lived node tears itself down.
#[cfg(feature = "experimental")]
const PUBLISH_FLUSH_GRACE: Duration = Duration::from_millis(500);

#[cfg(feature = "experimental")]
struct IdentityArgs {
    identity_file: PathBuf,
    force: bool,
}

#[cfg(feature = "experimental")]
struct JoinArgs {
    identity_file: PathBuf,
    ticket: String,
    peers: Vec<String>,
    status: String,
    message: Option<String>,
    progress: Option<u64>,
    artifact: Option<PathBuf>,
    loopback: bool,
}

#[cfg(feature = "experimental")]
enum Command {
    Identity(IdentityArgs),
    Join(JoinArgs),
}

// ---------------------------------------------------------------------------
// Argument parsing (dependency-light: hand-rolled, no `clap` — the CLI crate
// owns that dependency; this example stays minimal, see `README.md`).
// ---------------------------------------------------------------------------

#[cfg(feature = "experimental")]
fn usage() -> String {
    format!(
        "usage:\n  \
         example_agent identity [--identity-file <PATH>] [--force]\n  \
         example_agent join --ticket <ROOMTKT> [--identity-file <PATH>] \
         [--peer <ENDPOINT_ADDR>]... [--status <STATE>] [--message <TEXT>] \
         [--progress <0..100>] [--artifact <PATH>] [--loopback]\n\n\
         (--identity-file defaults to {DEFAULT_IDENTITY_FILE}; --status defaults to {DEFAULT_STATUS})"
    )
}

#[cfg(feature = "experimental")]
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value\n\n{}", usage()))
}

#[cfg(feature = "experimental")]
fn parse_args() -> anyhow::Result<Command> {
    let mut args = std::env::args().skip(1);
    let sub = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing subcommand\n\n{}", usage()))?;
    match sub.as_str() {
        "identity" => {
            let mut identity_file = PathBuf::from(DEFAULT_IDENTITY_FILE);
            let mut force = false;
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--identity-file" => {
                        identity_file = PathBuf::from(next_value(&mut args, "--identity-file")?);
                    }
                    "--force" => force = true,
                    other => anyhow::bail!("unrecognized argument {other:?}\n\n{}", usage()),
                }
            }
            Ok(Command::Identity(IdentityArgs {
                identity_file,
                force,
            }))
        }
        "join" => {
            let mut identity_file = PathBuf::from(DEFAULT_IDENTITY_FILE);
            let mut ticket = None;
            let mut peers = Vec::new();
            let mut status = DEFAULT_STATUS.to_owned();
            let mut message = None;
            let mut progress = None;
            let mut artifact = None;
            let mut loopback = false;
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--identity-file" => {
                        identity_file = PathBuf::from(next_value(&mut args, "--identity-file")?);
                    }
                    "--ticket" => ticket = Some(next_value(&mut args, "--ticket")?),
                    "--peer" => peers.push(next_value(&mut args, "--peer")?),
                    "--status" => status = next_value(&mut args, "--status")?,
                    "--message" => message = Some(next_value(&mut args, "--message")?),
                    "--progress" => {
                        let raw = next_value(&mut args, "--progress")?;
                        let pct: u64 = raw.parse().map_err(|_| {
                            anyhow::anyhow!("--progress must be an integer 0..=100 (got {raw:?})")
                        })?;
                        anyhow::ensure!(
                            pct <= 100,
                            "--progress must be an integer 0..=100 (got {pct})"
                        );
                        progress = Some(pct);
                    }
                    "--artifact" => {
                        artifact = Some(PathBuf::from(next_value(&mut args, "--artifact")?));
                    }
                    "--loopback" => loopback = true,
                    other => anyhow::bail!("unrecognized argument {other:?}\n\n{}", usage()),
                }
            }
            let ticket =
                ticket.ok_or_else(|| anyhow::anyhow!("--ticket is required\n\n{}", usage()))?;
            Ok(Command::Join(JoinArgs {
                identity_file,
                ticket,
                peers,
                status,
                message,
                progress,
                artifact,
                loopback,
            }))
        }
        other => anyhow::bail!("unrecognized subcommand {other:?}\n\n{}", usage()),
    }
}

// ---------------------------------------------------------------------------
// Identity persistence: two 32-byte seeds as lowercase hex, one per line,
// `0600` on Unix. Not the CLI's `identity.json`/`identity.secret` layout —
// the SDK does not expose that persistence, and reusing it would couple this
// example to a CLI-internal format (see `README.md`).
// ---------------------------------------------------------------------------

#[cfg(feature = "experimental")]
fn save_identity(
    path: &Path,
    identity: &SigningKey,
    device: &SigningKey,
    force: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        force || !path.exists(),
        "identity file {} already exists — pass --force to overwrite it (this would rotate the \
         key the admin already invited)",
        path.display()
    );
    // Hex-encode straight into the line we write; never copy the seed into a
    // longer-lived buffer than that (the `Zeroizing` wrapper only protects
    // what it owns).
    let contents = format!(
        "{}\n{}\n",
        hex::encode(*identity.to_seed()),
        hex::encode(*device.to_seed())
    );
    std::fs::write(path, contents)
        .with_context(|| format!("could not write identity file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("could not set permissions on {}", path.display()))?;
    }
    Ok(())
}

#[cfg(feature = "experimental")]
fn decode_seed_line(lines: &mut std::str::Lines<'_>, path: &Path) -> anyhow::Result<[u8; 32]> {
    let line = lines.next().ok_or_else(|| {
        anyhow::anyhow!(
            "identity file {} is malformed (missing a seed line)",
            path.display()
        )
    })?;
    let bytes = hex::decode(line.trim()).map_err(|_| {
        anyhow::anyhow!(
            "identity file {} is malformed (invalid hex)",
            path.display()
        )
    })?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        anyhow::anyhow!(
            "identity file {} is malformed (expected a 32-byte seed)",
            path.display()
        )
    })
}

#[cfg(feature = "experimental")]
fn load_identity(path: &Path) -> anyhow::Result<(SigningKey, SigningKey)> {
    let contents = std::fs::read_to_string(path).map_err(|_| {
        anyhow::anyhow!(
            "no identity file at {} — run `example_agent identity` first",
            path.display()
        )
    })?;
    let mut lines = contents.lines();
    let identity_seed = decode_seed_line(&mut lines, path)?;
    let device_seed = decode_seed_line(&mut lines, path)?;
    Ok((
        SigningKey::from_seed(&identity_seed),
        SigningKey::from_seed(&device_seed),
    ))
}

#[cfg(feature = "experimental")]
fn cmd_identity(path: &Path, force: bool) -> anyhow::Result<()> {
    // Idempotent by default: an existing file round-trips to the same id.
    // `--force` (or a missing file) generates + persists a fresh keypair.
    let identity = if path.exists() && !force {
        load_identity(path)?.0
    } else {
        let identity = SigningKey::generate();
        let device = SigningKey::generate();
        save_identity(path, &identity, &device, force)?;
        identity
    };
    println!("identity_id: {}", identity.identity_key());
    println!(
        "next: have the room admin run `iroh-rooms agent invite <ROOM_ID> {}`",
        identity.identity_key()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// `--peer` parsing: `<ENDPOINT_ID>[@<ip:port>[,<ip:port>...]]` — the
// deterministic loopback/LAN form `iroh-rooms room join --peer` also accepts.
// ---------------------------------------------------------------------------

#[cfg(feature = "experimental")]
fn parse_peer_addr(s: &str) -> anyhow::Result<EndpointAddr> {
    let s = s.trim();
    let (id_part, addr_part) = match s.split_once('@') {
        Some((id, rest)) => (id, Some(rest)),
        None => (s, None),
    };
    let id: EndpointId = id_part
        .trim()
        .parse()
        .map_err(|err| anyhow::anyhow!("invalid --peer endpoint id {id_part:?}: {err}"))?;
    let mut addr = EndpointAddr::new(id);
    if let Some(rest) = addr_part {
        for sock in rest.split(',') {
            let sock = sock.trim();
            if sock.is_empty() {
                continue;
            }
            let socket: SocketAddr = sock
                .parse()
                .map_err(|err| anyhow::anyhow!("invalid --peer socket address {sock:?}: {err}"))?;
            addr = addr.with_ip_addr(socket);
        }
    }
    Ok(addr)
}

/// One [`EndpointAddr`] per ticket discovery hint (MVP: the admin's device),
/// preferring a matching `--peer` (which may carry a deterministic
/// loopback/LAN socket address) over the bare id.
#[cfg(feature = "experimental")]
fn build_dial_set(
    ticket: &RoomInviteTicket,
    peer_addrs: &[EndpointAddr],
) -> anyhow::Result<Vec<EndpointAddr>> {
    let mut out = Vec::new();
    for dev in &ticket.discovery {
        let id = EndpointId::from_bytes(dev.as_bytes())?;
        let addr = peer_addrs
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .unwrap_or_else(|| EndpointAddr::new(id));
        out.push(addr);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Join by ticket + post agent.status (the core of the integration model).
// ---------------------------------------------------------------------------

#[cfg(feature = "experimental")]
async fn wait_for_invited(
    node: &Node,
    self_id: &IdentityKey,
    timeout: Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(snapshot) = node.snapshot().await {
                if snapshot.status(self_id).is_some() {
                    return;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "could not bootstrap the room membership within {timeout:?} — is the admin running \
             `iroh-rooms room tail <ROOM_ID> --accept-joins`?"
        )
    })
}

#[cfg(feature = "experimental")]
async fn wait_for_active(
    node: &Node,
    self_id: &IdentityKey,
    timeout: Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(timeout, async {
        loop {
            if let Ok(snapshot) = node.snapshot().await {
                if snapshot.is_active(self_id) {
                    return;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "published the join but did not observe the local Active transition within {timeout:?}"
        )
    })
}

#[cfg(feature = "experimental")]
fn connected_peer_count(node: &Node) -> usize {
    node.peer_states()
        .into_iter()
        .filter(|(_, state)| *state == PeerConnState::Connected)
        .count()
}

#[cfg(feature = "experimental")]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Import `artifact_path` into a local blob store, author + self-validate +
/// publish its `file.shared` reference, and return the `file_id` handle to
/// cite from the subsequent `agent.status` (issue G6, "optionally share one
/// artifact"). This session does not itself serve the bytes — see
/// `README.md`'s adaptation guide for wiring a long-running
/// `Node::spawn_room` provider, as `iroh-rooms room tail` does.
#[cfg(feature = "experimental")]
async fn share_artifact(
    node: &Node,
    identity: &SigningKey,
    device: &SigningKey,
    room_id: &iroh_rooms::room::RoomId,
    artifact_path: &Path,
) -> anyhow::Result<[u8; SHORT_ID_LEN]> {
    // iroh-blobs rejects a relative `add_path`.
    let abs_path = std::fs::canonicalize(artifact_path).with_context(|| {
        format!(
            "could not resolve --artifact path {}",
            artifact_path.display()
        )
    })?;

    let blobs_dir = std::env::temp_dir().join("iroh-rooms-example-agent-blobs");
    let store = BlobStore::open(&blobs_dir)
        .await
        .with_context(|| format!("could not open blob store at {}", blobs_dir.display()))?;
    let import = store
        .import_path(&abs_path)
        .await
        .with_context(|| format!("could not import {}", abs_path.display()))?;
    // The FsStore holds an exclusive lock; close it before this process exits.
    store.close().await.context("could not close blob store")?;

    let mut file_id = [0u8; SHORT_ID_LEN];
    getrandom::fill(&mut file_id).map_err(|e| anyhow::anyhow!("OS CSPRNG unavailable: {e}"))?;
    let name = abs_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact")
        .to_owned();

    let heads = node.heads().await?;
    let wire = build_file_shared(
        identity,
        device,
        room_id,
        file_id,
        &name,
        "application/octet-stream",
        import.size_bytes,
        HashRef::from_bytes(import.hash),
        None,
        &[],
        &heads,
        now_ms(),
    );
    validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(*room_id)).map_err(
        |reason| anyhow::anyhow!("freshly built file.shared failed validation: {reason:?}"),
    )?;
    node.publish(wire.to_bytes())
        .await
        .context("could not publish the file.shared reference")?;
    println!(
        "shared artifact: {name} ({} bytes, blake3:{})",
        import.size_bytes,
        hex::encode(import.hash)
    );
    println!(
        "note: this short-lived session does not itself serve the blob's bytes — a \
         long-running `Node::spawn_room` session (as `iroh-rooms room tail` runs) is needed to \
         serve fetches; see README.md"
    );

    Ok(file_id)
}

/// The post-bring-up half of [`cmd_join`]: wait for the connection + the
/// membership pull, build + self-validate + publish the join, confirm local
/// `Active`, optionally share an artifact, then post the `agent.status`.
#[cfg(feature = "experimental")]
async fn run_join(
    node: &Node,
    ticket: &RoomInviteTicket,
    identity: &SigningKey,
    device: &SigningKey,
    admin_id: EndpointId,
    args: &JoinArgs,
) -> anyhow::Result<()> {
    node.wait_for_state(admin_id, PeerConnState::Connected, JOIN_TIMEOUT)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "could not connect to the room admin within {JOIN_TIMEOUT:?} — is \
                 `iroh-rooms room tail <ROOM_ID> --accept-joins` running?"
            )
        })?;

    let self_id = identity.identity_key();
    // The membership sub-DAG is never windowed, so this always converges once
    // connected — a real agent must *confirm* the invite before authoring the
    // join, not just sleep (spec D6 step 4.7).
    wait_for_invited(node, &self_id, JOIN_TIMEOUT).await?;

    let heads = node.heads().await?;
    let binding = DeviceBinding::create(&ticket.room_id, identity, device.device_key());
    let wire = build_member_joined(
        identity,
        device,
        &ticket.room_id,
        &ticket.invite_id,
        &ticket.capability_secret,
        &ticket.role,
        binding,
        Some(AGENT_DISPLAY_NAME),
        &heads,
        now_ms(),
    );
    validate_wire_bytes(
        &wire.to_bytes(),
        &ValidationContext::for_room(ticket.room_id),
    )
    .map_err(|reason| {
        anyhow::anyhow!("freshly built member.joined failed validation: {reason:?}")
    })?;
    node.publish(wire.to_bytes())
        .await
        .context("could not publish the join")?;

    wait_for_active(node, &self_id, JOIN_TIMEOUT).await?;
    println!("joined: {} role={}", ticket.room_id, ticket.role);

    let mut artifact_ids: Vec<[u8; SHORT_ID_LEN]> = Vec::new();
    if let Some(artifact_path) = &args.artifact {
        let file_id =
            share_artifact(node, identity, device, &ticket.room_id, artifact_path).await?;
        artifact_ids.push(file_id);
    }

    let heads = node.heads().await?;
    let wire = build_agent_status(
        identity,
        device,
        &ticket.room_id,
        &args.status,
        args.message.as_deref(),
        &artifact_ids,
        args.progress,
        &heads,
        now_ms(),
    );
    let validated = validate_wire_bytes(
        &wire.to_bytes(),
        &ValidationContext::for_room(ticket.room_id),
    )
    .map_err(|reason| {
        anyhow::anyhow!("freshly built agent.status failed validation: {reason:?}")
    })?;
    node.publish(wire.to_bytes())
        .await
        .context("could not publish the status")?;

    // The same honesty lines `iroh-rooms agent status` prints: Iroh Rooms is
    // best-effort peer-to-peer, so "stored" (always true once published) is
    // distinct from "delivered" (best-effort, only meaningful while a peer is
    // actually connected — PRD §14).
    println!("status: {}", validated.event_id);
    println!("room:   {}", ticket.room_id);
    println!("from:   {self_id}");
    println!("stored: yes");
    let connected = connected_peer_count(node);
    if connected == 0 {
        println!("delivered: 0 (no peers online — stored locally only)");
    } else {
        println!("delivered: {connected} connected peer(s)");
    }

    Ok(())
}

#[cfg(feature = "experimental")]
async fn cmd_join(args: JoinArgs) -> anyhow::Result<()> {
    let ticket: RoomInviteTicket = args.ticket.trim().parse()?;

    let (identity, device) = load_identity(&args.identity_file)?;
    anyhow::ensure!(
        identity.identity_key() == ticket.invitee_key,
        "this identity ({}) was not the one invited (the ticket is bound to {}) — have the \
         admin run `iroh-rooms agent invite <ROOM_ID> <ID>` with the id printed by \
         `example_agent identity`",
        identity.identity_key(),
        ticket.invitee_key
    );

    // D4: the only capability this agent holds is the room membership the
    // ticket granted. Admission is seeded solely from the ticket's discovery
    // hint (the admin's device) — this node dials/accepts no one else until
    // the fold teaches it otherwise.
    let mut admission = AllowlistAdmission::new();
    for dev in &ticket.discovery {
        admission = admission.bind_device(
            EndpointId::from_bytes(dev.as_bytes())?,
            ticket.inviter_identity,
        );
    }
    admission = admission.set_active(ticket.inviter_identity);

    let peer_addrs = args
        .peers
        .iter()
        .map(|s| parse_peer_addr(s))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let dial_set = build_dial_set(&ticket, &peer_addrs)?;
    let admin_id = dial_set.first().map(|a| a.id).ok_or_else(|| {
        anyhow::anyhow!(
            "the invite ticket carries no admin discovery hint; cannot reach the room admin"
        )
    })?;

    let engine = SyncEngine::open(
        EventStore::open_in_memory()?,
        ticket.room_id,
        SyncConfig::default(),
    )
    .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let net_cfg = NetConfig {
        mode: if args.loopback {
            NetMode::Loopback
        } else {
            NetMode::RealNetwork
        },
        ..NetConfig::default()
    };
    let node = Node::spawn(
        SecretKey::from_bytes(&device.to_seed()),
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        net_cfg,
        DEFAULT_TICK,
    )
    .await
    .context("could not bring up the network node")?;

    for addr in dial_set {
        node.connect_to(addr);
    }

    let outcome = run_join(&node, &ticket, &identity, &device, admin_id, &args).await;

    tokio::time::sleep(PUBLISH_FLUSH_GRACE).await;
    node.shutdown()
        .await
        .context("could not shut down the network node")?;
    outcome
}

#[cfg(feature = "experimental")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match parse_args()? {
        Command::Identity(args) => cmd_identity(&args.identity_file, args.force),
        Command::Join(args) => cmd_join(args).await,
    }
}

#[cfg(not(feature = "experimental"))]
fn main() {
    eprintln!("this example requires `--features experimental` (see the module doc header)");
}
